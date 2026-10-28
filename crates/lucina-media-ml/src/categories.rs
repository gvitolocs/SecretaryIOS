//! Automatic photo categories: what kind of picture a photo is (a screenshot, a receipt, a meme,
//! a pet...), decided once, in the background, from a typed record of facts about it.
//!
//! The pipeline has three pure steps, none of which looks at pixels or names a file:
//!
//! 1. [`PromptVectors`] scores a photo's stored vector against a few text prompts per category
//!    (zero-shot, with whichever [`Embedder`] is current) and gives a [`ZeroShot`].
//! 2. [`Features::build`] puts that score next to the cheap facts the catalog already knows (size,
//!    format, whether it looks like a screen, camera data, where it came from, a [`NameHint`] that
//!    replaces the file name) into one versioned record, [`FEATURES_VERSION`].
//! 3. A [`Decider`] turns the record into a [`Decision`]. [`RulesDecider`] is the complete
//!    rules-based one; a model-backed decider (JEV) implements the same trait and is chosen where
//!    the service installs its decider (`lucina-media`, `categories::install_decider`).
//!
//! The store ([`crate::categories_store`]) keeps the decision with the model, the features version
//! and the decider that made it, so a change of any of the three decides the photo again, and the
//! owner's corrections, which no decider ever overwrites.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::vector;
use crate::{Embedder, MlError};

/// The version of the [`Features`] record. Bump it when a field's meaning changes: every photo is
/// then decided again.
pub const FEATURES_VERSION: u32 = 1;

/// Scales a cosine difference into the softmax that turns category maxima into 0..1 scores.
/// Image-text cosines of the models in use sit between 0.05 and 0.30, a gap of 0.03 between two
/// categories is a clear preference at this scale (a logit gap of 3, 95 to 5).
pub const ZERO_SHOT_SCALE: f32 = 100.0;

// ---- categories --------------------------------------------------------------------------------

/// What a photo is. `Photo` is everything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Screenshot,
    Receipt,
    /// Papers, IDs, forms.
    Document,
    /// Notes, whiteboards, lecture slides.
    Note,
    /// Boarding passes, event tickets, QR codes.
    Ticket,
    /// Maps and directions.
    Map,
    /// Memes and saved images from chats and shares.
    Meme,
    Food,
    Pet,
    Photo,
}

impl Category {
    /// Every category, in the order the apps show them.
    pub const ALL: [Category; 10] = [
        Category::Screenshot,
        Category::Receipt,
        Category::Document,
        Category::Note,
        Category::Ticket,
        Category::Map,
        Category::Meme,
        Category::Food,
        Category::Pet,
        Category::Photo,
    ];

    /// Categories that stay out of Memories, Places and share suggestions: they show things
    /// nobody wants on a map or in a "remember this day" strip.
    pub const SENSITIVE: [Category; 4] = [
        Category::Document,
        Category::Receipt,
        Category::Screenshot,
        Category::Ticket,
    ];

    /// Categories that count as real photos in "on this day".
    pub const REAL_PHOTOS: [Category; 3] = [Category::Photo, Category::Food, Category::Pet];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Screenshot => "screenshot",
            Category::Receipt => "receipt",
            Category::Document => "document",
            Category::Note => "note",
            Category::Ticket => "ticket",
            Category::Map => "map",
            Category::Meme => "meme",
            Category::Food => "food",
            Category::Pet => "pet",
            Category::Photo => "photo",
        }
    }

    pub fn parse(s: &str) -> Option<Category> {
        Category::ALL.into_iter().find(|c| c.as_str() == s)
    }

    pub fn index(self) -> usize {
        Category::ALL
            .iter()
            .position(|c| *c == self)
            .unwrap_or(Category::ALL.len() - 1)
    }

    pub fn is_sensitive(self) -> bool {
        Category::SENSITIVE.contains(&self)
    }

    /// The prompts embedded for the zero-shot score: several phrasings per category, so no single
    /// wording decides. English only; the text towers in use understand other languages, and
    /// the prompts describe what the picture shows, not what the owner calls it.
    pub fn prompts(self) -> &'static [&'static str] {
        match self {
            Category::Screenshot => &[
                "a screenshot of a phone screen",
                "a screenshot of an app interface with buttons and text",
                "a screenshot of a website on a computer screen",
                "a screen capture of a chat conversation",
            ],
            Category::Receipt => &[
                "a photo of a shop receipt with prices",
                "a paper receipt with a list of purchased items and a total",
                "a restaurant bill on the table",
                "an invoice with amounts and a total",
            ],
            Category::Document => &[
                "a photo of a printed document",
                "a scanned page of a letter or contract",
                "an identity card or passport",
                "a filled-in paper form",
            ],
            Category::Note => &[
                "a photo of handwritten notes",
                "a whiteboard with writing and diagrams",
                "a lecture slide with bullet points",
                "a page of a notebook with handwriting",
            ],
            Category::Ticket => &[
                "a boarding pass",
                "an event ticket with a QR code",
                "a QR code on a screen or a paper",
                "a train or concert ticket",
            ],
            Category::Map => &[
                "a map with streets and a route",
                "a screenshot of a map app with directions",
                "a hiking trail map",
                "a metro or city map",
            ],
            Category::Meme => &[
                "a meme with a caption in large text",
                "a funny image with text overlay shared in a chat",
                "a cartoon or comic strip",
                "a saved internet picture with a joke",
            ],
            Category::Food => &[
                "a photo of food on a plate",
                "a meal in a restaurant",
                "a dessert or a cake",
                "a dish being cooked in the kitchen",
            ],
            Category::Pet => &[
                "a photo of a pet",
                "a close-up photo of a dog",
                "a photo of a cat",
                "a pet animal at home",
            ],
            Category::Photo => &[
                "a photo of people smiling",
                "a landscape photo of nature",
                "a photo of a city street or a building",
                "a holiday photo taken outdoors",
                "a family photo at home",
            ],
        }
    }
}

// ---- zero-shot scores --------------------------------------------------------------------------

/// The zero-shot result for one photo: per category the best cosine against its prompts and the
/// score (0..1) after a softmax over the categories' bests.
#[derive(Clone, Debug, PartialEq)]
pub struct ZeroShot {
    pub cosines: [f32; Category::ALL.len()],
    pub scores: [f32; Category::ALL.len()],
}

impl ZeroShot {
    /// Scores from per-category best cosines (in [`Category::ALL`] order): softmax of
    /// `cosine * scale`, so the scores add up to 1.
    pub fn from_cosines(cosines: [f32; Category::ALL.len()], scale: f32) -> ZeroShot {
        let max = cosines.iter().copied().fold(f32::MIN, f32::max);
        let mut scores = [0f32; Category::ALL.len()];
        let mut sum = 0f32;
        for (s, c) in scores.iter_mut().zip(cosines) {
            *s = ((c - max) * scale).exp();
            sum += *s;
        }
        for s in scores.iter_mut() {
            *s /= sum;
        }
        ZeroShot { cosines, scores }
    }

    pub fn score(&self, c: Category) -> f32 {
        self.scores[c.index()]
    }

    pub fn cosine(&self, c: Category) -> f32 {
        self.cosines[c.index()]
    }

    /// The category with the highest score (the first of equals in [`Category::ALL`] order).
    pub fn top(&self) -> Category {
        let mut best = Category::ALL[0];
        for c in Category::ALL {
            if self.score(c) > self.score(best) {
                best = c;
            }
        }
        best
    }
}

/// The prompts of every category as unit vectors of one model. Built once per model (the service
/// caches it) with [`PromptVectors::build`].
#[derive(Clone, Debug)]
pub struct PromptVectors {
    model: String,
    dim: usize,
    scale: f32,
    /// Per category, in [`Category::ALL`] order.
    vectors: Vec<Vec<Vec<f32>>>,
}

impl PromptVectors {
    /// Embeds every prompt with `embedder`. Fails if the text tower does.
    pub fn build(embedder: &dyn Embedder) -> Result<PromptVectors, MlError> {
        let dim = embedder.dim();
        let mut vectors = Vec::new();
        for c in Category::ALL {
            let mut per = Vec::new();
            for p in c.prompts() {
                let mut v = embedder.embed_text(p)?;
                if v.len() != dim || !vector::normalize(&mut v) {
                    return Err(MlError::Failed(format!(
                        "the text tower returned a bad vector for a {} prompt",
                        c.as_str()
                    )));
                }
                per.push(v);
            }
            vectors.push(per);
        }
        Ok(PromptVectors {
            model: embedder.model().to_string(),
            dim,
            scale: ZERO_SHOT_SCALE,
            vectors,
        })
    }

    /// Prompt vectors made by hand (tests): `vectors[i]` are the prompts of `Category::ALL[i]`.
    /// Each is normalised; an empty or wrong-sized one is an error.
    pub fn from_vectors(
        model: &str,
        mut vectors: Vec<Vec<Vec<f32>>>,
    ) -> Result<PromptVectors, MlError> {
        if vectors.len() != Category::ALL.len() || vectors.iter().any(|v| v.is_empty()) {
            return Err(MlError::Failed(
                "every category needs at least one prompt vector".into(),
            ));
        }
        let dim = vectors[0][0].len();
        for v in vectors.iter_mut().flatten() {
            if v.len() != dim || !vector::normalize(v) {
                return Err(MlError::Failed("bad prompt vector".into()));
            }
        }
        Ok(PromptVectors {
            model: model.to_string(),
            dim,
            scale: ZERO_SHOT_SCALE,
            vectors,
        })
    }

    pub fn with_scale(mut self, scale: f32) -> PromptVectors {
        self.scale = scale;
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// The zero-shot score of a photo's vector; `None` if it is not of this model's size or not
    /// a usable vector.
    pub fn score(&self, photo: &[f32]) -> Option<ZeroShot> {
        if photo.len() != self.dim {
            return None;
        }
        let mut p = photo.to_vec();
        if !vector::normalize(&mut p) {
            return None;
        }
        let mut cosines = [0f32; Category::ALL.len()];
        for (i, per) in self.vectors.iter().enumerate() {
            cosines[i] = per
                .iter()
                .map(|v| vector::dot(&p, v))
                .fold(f32::MIN, f32::max);
        }
        Some(ZeroShot::from_cosines(cosines, self.scale))
    }
}

// ---- facts about a photo -----------------------------------------------------------------------

/// What the original's name looks like, without keeping the name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameHint {
    /// "Screenshot ...", "Screen Shot ...", "Schermata ...", "Skærmbillede ...".
    Screenshot,
    /// A scanner app's output ("Scan ...", "CamScanner ...", "Scansione ...").
    Scan,
    /// A chat or share app's naming (WhatsApp, Telegram, Signal, Messenger, received_...).
    Chat,
    /// A camera's default name (IMG_0001, DSC01234, PXL_..., a date and time).
    Camera,
    Other,
}

impl NameHint {
    /// The hint for a file name (or a title). Only the hint is kept; nothing of the name is.
    pub fn from_name(name: &str) -> NameHint {
        let n = name.trim().to_lowercase();
        if n.is_empty() {
            return NameHint::Other;
        }
        let tokens: Vec<&str> = n
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .collect();
        let first = tokens.first().copied().unwrap_or("");
        let has = |t: &str| tokens.contains(&t);
        let starts = |p: &str| first.starts_with(p);

        if has("screenshot")
            || starts("screenshot")
            || starts("schermata")
            || starts("skærmbillede")
            || starts("skaermbillede")
            || (has("screen") && (has("shot") || has("capture") || has("recording")))
            || n.starts_with("simulator screen shot")
        {
            return NameHint::Screenshot;
        }
        if [
            "scan",
            "scansione",
            "scanned",
            "camscanner",
            "scannet",
            "scanner",
        ]
        .iter()
        .any(|t| has(t))
            || starts("scan")
                && first
                    .strip_prefix("scan")
                    .is_some_and(|rest| rest.bytes().all(|b| b.is_ascii_digit()))
        {
            return NameHint::Scan;
        }
        let wa_number = n.find("-wa").is_some_and(|i| {
            let tail = &n[i + 3..];
            tail.len() >= 4 && tail.bytes().take(4).all(|b| b.is_ascii_digit())
        });
        if n.contains("whatsapp")
            || wa_number
            || has("telegram")
            || has("signal")
            || has("messenger")
            || has("viber")
            || starts("received")
            || starts("fb_img")
            || (starts("photo") && n.contains('@'))
            || starts("fbimg")
        {
            return NameHint::Chat;
        }
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        let camera_prefix = [
            "img", "dsc", "dscf", "dscn", "pxl", "mvimg", "gopr", "dji", "p",
        ]
        .iter()
        .any(|p| {
            first
                .strip_prefix(p)
                .is_some_and(|rest| rest.len() >= 3 && rest.bytes().all(|b| b.is_ascii_digit()))
        }) || (["img", "dsc", "pxl", "dji", "mvimg"]
            .iter()
            .any(|p| starts(p))
            && tokens.get(1).is_some_and(|t| digits(t)));
        // An Android camera timestamp: 20240131_142501.
        let stamp = digits(first) && first.len() == 8 && tokens.get(1).is_some_and(|t| digits(t));
        if camera_prefix || stamp {
            return NameHint::Camera;
        }
        NameHint::Other
    }
}

/// The picture's format, from its MIME type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Heic,
    Webp,
    Gif,
    Other,
}

impl ImageFormat {
    pub fn from_mime(mime: &str) -> ImageFormat {
        match mime.trim().to_ascii_lowercase().as_str() {
            "image/png" => ImageFormat::Png,
            "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
            "image/heic" | "image/heif" => ImageFormat::Heic,
            "image/webp" => ImageFormat::Webp,
            "image/gif" => ImageFormat::Gif,
            _ => ImageFormat::Other,
        }
    }
}

/// Where the photo came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Backed up from a phone or another registered device.
    PhoneBackup,
    /// Arrived through chat or a share sheet.
    Chat,
    /// Made or found by Lucina.
    Lucina,
    /// Imported from a folder (the Drive's photo folders, a Takeout, old Nextcloud files).
    DriveFolder,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
    Other,
}

impl MediaKind {
    pub fn parse(kind: &str) -> MediaKind {
        match kind {
            "photo" => MediaKind::Photo,
            "video" => MediaKind::Video,
            _ => MediaKind::Other,
        }
    }
}

/// Pixel sizes (short side, long side) of common screens: phones, tablets and desktops, the
/// resolutions a screenshot has when it is the whole screen. A size matches in either orientation.
pub const SCREEN_SIZES: &[(u32, u32)] = &[
    // iPhone
    (640, 1136),
    (750, 1334),
    (828, 1792),
    (1080, 1920),
    (1125, 2436),
    (1170, 2532),
    (1179, 2556),
    (1206, 2622),
    (1242, 2208),
    (1242, 2688),
    (1284, 2778),
    (1290, 2796),
    (1320, 2868),
    // Android phones
    (720, 1280),
    (720, 1520),
    (720, 1600),
    (1080, 2160),
    (1080, 2220),
    (1080, 2240),
    (1080, 2280),
    (1080, 2340),
    (1080, 2400),
    (1080, 2408),
    (1080, 2412),
    (1440, 2960),
    (1440, 3040),
    (1440, 3088),
    (1440, 3120),
    (1440, 3200),
    (1344, 2992),
    // Tablets
    (1488, 2266),
    (1536, 2048),
    (1620, 2160),
    (1640, 2360),
    (1668, 2224),
    (1668, 2388),
    (2048, 2732),
    (2064, 2752),
    (800, 1280),
    (1200, 1920),
    (1600, 2560),
    // Desktops and laptops (1080 by 1920 and the 16:10 sizes above are shared with the tablets)
    (768, 1366),
    (900, 1440),
    (900, 1600),
    (864, 1536),
    (982, 1512),
    (1117, 1728),
    (1152, 2048),
    (1440, 2560),
    (1440, 3440),
    (1664, 2560),
    (1692, 3008),
    (1800, 2880),
    (1912, 2940),
    (1964, 3024),
    (2160, 3840),
    (2224, 3420),
    (2234, 3456),
    (2880, 5120),
];

/// Whether `w`×`h` is the size of a whole screen, in either orientation.
pub fn is_screen_sized(w: u32, h: u32) -> bool {
    let (short, long) = (w.min(h), w.max(h));
    SCREEN_SIZES.iter().any(|&(s, l)| s == short && l == long)
}

// ---- the feature record ------------------------------------------------------------------------

/// What the catalog knows about a photo, before any model: the input of [`Features::build`].
#[derive(Clone, Debug)]
pub struct Facts<'a> {
    pub kind: &'a str,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub mime: &'a str,
    pub source_kind: SourceKind,
    /// The original file name, or the title; only a [`NameHint`] is kept.
    pub name: Option<&'a str>,
    /// The camera make and model from EXIF, as the catalog stored it.
    pub camera: Option<&'a str>,
    pub has_gps: bool,
}

/// The typed record a [`Decider`] decides from. It holds no file name, no place, no pixels:
/// it can be logged, stored and sent to a text-only model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Features {
    pub version: u32,
    pub kind: MediaKind,
    /// Zero-shot score per category, 0..1 (softmax over the categories' best prompt), 3 decimals.
    pub zero_shot: BTreeMap<Category, f32>,
    /// The category with the highest zero-shot score and its raw cosine.
    pub top_category: Category,
    pub top_cosine: f32,
    /// 0 when the catalog does not know the size.
    pub width: u32,
    pub height: u32,
    /// Width over height (as stored, before any rotation the viewer applies); 0 when unknown.
    pub aspect: f32,
    /// The size of a whole phone, tablet or desktop screen, in either orientation.
    pub screen_sized: bool,
    /// The file has camera make or model in its EXIF.
    pub camera_exif: bool,
    pub format: ImageFormat,
    pub source_kind: SourceKind,
    pub name_hint: NameHint,
    pub has_gps: bool,
}

fn round3(x: f32) -> f32 {
    (x * 1000.0).round() / 1000.0
}

impl Features {
    pub fn build(f: &Facts, zs: &ZeroShot) -> Features {
        let dim = |v: Option<i64>| v.filter(|x| *x > 0).and_then(|x| u32::try_from(x).ok());
        let (w, h) = (dim(f.width).unwrap_or(0), dim(f.height).unwrap_or(0));
        let top = zs.top();
        Features {
            version: FEATURES_VERSION,
            kind: MediaKind::parse(f.kind),
            zero_shot: Category::ALL
                .into_iter()
                .map(|c| (c, round3(zs.score(c))))
                .collect(),
            top_category: top,
            top_cosine: round3(zs.cosine(top)),
            width: w,
            height: h,
            aspect: if w > 0 && h > 0 {
                round3(w as f32 / h as f32)
            } else {
                0.0
            },
            screen_sized: w > 0 && h > 0 && is_screen_sized(w, h),
            camera_exif: f.camera.is_some_and(|c| !c.trim().is_empty()),
            format: ImageFormat::from_mime(f.mime),
            source_kind: f.source_kind,
            name_hint: f.name.map(NameHint::from_name).unwrap_or(NameHint::Other),
            has_gps: f.has_gps,
        }
    }

    pub fn score(&self, c: Category) -> f32 {
        self.zero_shot.get(&c).copied().unwrap_or(0.0)
    }
}

// ---- deciders ----------------------------------------------------------------------------------

/// Who made an automatic decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeciderKind {
    Rules,
    Jev,
}

impl DeciderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeciderKind::Rules => "rules",
            DeciderKind::Jev => "jev",
        }
    }
}

/// One decision: the category, how sure the decider is (0..1) and a short reason.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub category: Category,
    pub confidence: f32,
    /// A short id of the rule or the model's reason, for debugging (never a file name).
    pub reason: String,
}

/// Turns a feature record into a category. [`RulesDecider`] is the rules-based one; another
/// decider (a typed decision model) is a second implementation, installed with
/// `lucina-media`'s `categories::install_decider`.
pub trait Decider: Send + Sync {
    fn kind(&self) -> DeciderKind;
    /// Stored with every decision: `kind` and the version of the rules or model. A decision made
    /// by another id is made again.
    fn id(&self) -> String;
    /// An error leaves the photo undecided (the service tries again after a restart), except
    /// [`MlError::Unavailable`]: the decider is temporarily out (quota, outage), the photo stays
    /// pending and the step stops for this round.
    fn decide(&self, features: &Features) -> Result<Decision, MlError>;
    /// Whether it can decide now. While it cannot, the step decides with the rules, but only
    /// photos that have no decision yet: an outage never replaces this decider's answers.
    fn ready(&self) -> bool {
        true
    }
}

/// The thresholds of the rules, in one place so tests and tuning see them.
#[derive(Clone, Debug, PartialEq)]
pub struct Thresholds {
    /// Minimum zero-shot score of a sensitive category (document, receipt, ticket): a missed
    /// receipt lands on the map, a false one only misses a memory, so the bar is lower.
    pub sensitive: f32,
    pub note: f32,
    pub map: f32,
    pub screenshot: f32,
    pub food_pet: f32,
    /// A meme from chat or share with no camera data.
    pub meme_chat: f32,
    pub meme: f32,
    /// Added for categories that are not sensitive when the photo has a GPS position (a
    /// geotagged picture is most likely a real photo).
    pub gps_bump: f32,
    /// How far the category's score must be above `photo`'s: sensitive ones and the rest.
    pub margin_sensitive: f32,
    pub margin: f32,
    /// Minimum screenshot score for a screen-sized picture without camera data that is not a PNG.
    pub screenshot_lookalike: f32,
    /// Minimum score for a scanner app's picture to take the category instead of "document".
    pub scan_pick: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds {
            sensitive: 0.40,
            note: 0.45,
            map: 0.50,
            screenshot: 0.50,
            food_pet: 0.50,
            meme_chat: 0.40,
            meme: 0.60,
            gps_bump: 0.05,
            margin_sensitive: 0.10,
            margin: 0.15,
            screenshot_lookalike: 0.50,
            scan_pick: 0.25,
        }
    }
}

/// The rules-based decider. Rules run in this order, the first that applies decides:
///
/// | id | applies when | category | confidence |
/// |---|---|---|---|
/// | `not-a-photo` | the kind is not a photo | photo | 0.5 |
/// | `screenshot-screen-size` | screen-sized, no camera EXIF, PNG or name hint screenshot | screenshot | 0.95 |
/// | `screenshot-name` | name hint screenshot, no camera EXIF | screenshot | 0.90 |
/// | `scan-name` | name hint scan | best of document, receipt, note, ticket by score (at least `scan_pick`), else document | max(score, 0.6) |
/// | `screenshot-lookalike` | screen-sized, no camera EXIF, screenshot score at least `screenshot_lookalike` | screenshot | the score |
/// | `zero-shot` | the best other category passes its threshold and margin over photo (camera EXIF rules out screenshot and meme) | that category | its score |
/// | `default` | nothing above | photo | the larger of photo's score and 1 minus the best other |
#[derive(Clone, Debug, Default)]
pub struct RulesDecider {
    pub thresholds: Thresholds,
}

/// The version of the rules table: change it with the rules.
pub const RULES_VERSION: u32 = 1;

impl RulesDecider {
    fn min_score(&self, c: Category, f: &Features) -> f32 {
        let t = &self.thresholds;
        let base = match c {
            Category::Document | Category::Receipt | Category::Ticket => t.sensitive,
            Category::Note => t.note,
            Category::Map => t.map,
            Category::Screenshot => t.screenshot,
            Category::Food | Category::Pet => t.food_pet,
            Category::Meme => {
                if f.source_kind == SourceKind::Chat && !f.camera_exif {
                    t.meme_chat
                } else {
                    t.meme
                }
            }
            Category::Photo => 0.0,
        };
        if f.has_gps && !c.is_sensitive() {
            base + t.gps_bump
        } else {
            base
        }
    }

    fn margin(&self, c: Category) -> f32 {
        if c.is_sensitive() {
            self.thresholds.margin_sensitive
        } else {
            self.thresholds.margin
        }
    }

    /// The decision and the id of the rule that made it.
    pub fn explain(&self, f: &Features) -> Decision {
        let t = &self.thresholds;
        let d = |category, confidence: f32, reason: &str| Decision {
            category,
            confidence: round3(confidence.clamp(0.0, 1.0)),
            reason: reason.into(),
        };
        if f.kind != MediaKind::Photo {
            return d(Category::Photo, 0.5, "not-a-photo");
        }
        if f.screen_sized
            && !f.camera_exif
            && (f.format == ImageFormat::Png || f.name_hint == NameHint::Screenshot)
        {
            return d(Category::Screenshot, 0.95, "screenshot-screen-size");
        }
        if f.name_hint == NameHint::Screenshot && !f.camera_exif {
            return d(Category::Screenshot, 0.90, "screenshot-name");
        }
        if f.name_hint == NameHint::Scan {
            let pick = [
                Category::Document,
                Category::Receipt,
                Category::Note,
                Category::Ticket,
            ]
            .into_iter()
            .fold(Category::Document, |best, c| {
                if f.score(c) > f.score(best) {
                    c
                } else {
                    best
                }
            });
            let (cat, score) = if f.score(pick) >= t.scan_pick {
                (pick, f.score(pick))
            } else {
                (Category::Document, 0.0)
            };
            return d(cat, score.max(0.6), "scan-name");
        }
        if f.screen_sized
            && !f.camera_exif
            && f.score(Category::Screenshot) >= t.screenshot_lookalike
        {
            return d(
                Category::Screenshot,
                f.score(Category::Screenshot),
                "screenshot-lookalike",
            );
        }
        let photo_score = f.score(Category::Photo);
        let mut best: Option<(Category, f32)> = None;
        let mut best_other = 0f32;
        for c in Category::ALL {
            if c == Category::Photo {
                continue;
            }
            let s = f.score(c);
            best_other = best_other.max(s);
            // A camera's picture is not a screen capture and not a saved image.
            if f.camera_exif && matches!(c, Category::Screenshot | Category::Meme) {
                continue;
            }
            if s >= self.min_score(c, f)
                && s - photo_score >= self.margin(c)
                && best.is_none_or(|(_, b)| s > b)
            {
                best = Some((c, s));
            }
        }
        match best {
            Some((c, s)) => d(c, s, "zero-shot"),
            None => d(
                Category::Photo,
                photo_score.max(1.0 - best_other),
                "default",
            ),
        }
    }
}

impl Decider for RulesDecider {
    fn kind(&self) -> DeciderKind {
        DeciderKind::Rules
    }

    fn id(&self) -> String {
        format!("rules-{RULES_VERSION}")
    }

    fn decide(&self, features: &Features) -> Result<Decision, MlError> {
        Ok(self.explain(features))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = Category::ALL.len();

    /// Unit vectors on N axes: category i is axis i.
    fn axis(i: usize) -> Vec<f32> {
        let mut v = vec![0.0; N];
        v[i] = 1.0;
        v
    }

    fn prompts() -> PromptVectors {
        let per: Vec<Vec<Vec<f32>>> = (0..N).map(|i| vec![axis(i), axis(i)]).collect();
        PromptVectors::from_vectors("axes", per).unwrap()
    }

    /// A photo vector pointing `deg` degrees from category `from` towards category `towards`.
    fn mix(from: Category, towards: Category, deg: f32) -> Vec<f32> {
        let (a, b) = (axis(from.index()), axis(towards.index()));
        let (c, s) = (deg.to_radians().cos(), deg.to_radians().sin());
        a.iter().zip(&b).map(|(x, y)| c * x + s * y).collect()
    }

    fn facts<'a>() -> Facts<'a> {
        Facts {
            kind: "photo",
            width: Some(4032),
            height: Some(3024),
            mime: "image/jpeg",
            source_kind: SourceKind::PhoneBackup,
            name: Some("IMG_0001.JPG"),
            camera: Some("Apple iPhone"),
            has_gps: false,
        }
    }

    /// Features for a photo that looks like `look` (zero-shot) with the facts changed by `f`.
    fn feat(look: Category, f: impl FnOnce(&mut Facts)) -> Features {
        let zs = prompts().score(&axis(look.index())).unwrap();
        let mut fa = facts();
        f(&mut fa);
        Features::build(&fa, &zs)
    }

    #[test]
    fn categories_round_trip_and_know_their_sets() {
        assert_eq!(Category::ALL.len(), 10);
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
            assert_eq!(Category::ALL[c.index()], c);
            assert!(!c.prompts().is_empty());
        }
        assert_eq!(Category::parse("Receipt"), None);
        assert_eq!(Category::parse(""), None);
        let sensitive: Vec<&str> = Category::ALL
            .into_iter()
            .filter(|c| c.is_sensitive())
            .map(Category::as_str)
            .collect();
        assert_eq!(sensitive, ["screenshot", "receipt", "document", "ticket"]);
        assert!(Category::SENSITIVE
            .iter()
            .all(|c| !Category::REAL_PHOTOS.contains(c)));
        let json = serde_json::to_string(&Category::Receipt).unwrap();
        assert_eq!(json, "\"receipt\"");
    }

    #[test]
    fn every_prompt_belongs_to_one_category() {
        let mut seen = std::collections::HashSet::new();
        for c in Category::ALL {
            for p in c.prompts() {
                assert!(seen.insert(*p), "duplicate prompt {p}");
            }
        }
    }

    #[test]
    fn zero_shot_picks_the_nearest_category() {
        let pv = prompts();
        for c in Category::ALL {
            let zs = pv.score(&axis(c.index())).unwrap();
            assert_eq!(zs.top(), c);
            assert!(zs.score(c) > 0.99, "{c:?} {}", zs.score(c));
            assert!((zs.scores.iter().sum::<f32>() - 1.0).abs() < 1e-4);
            assert!((zs.cosine(c) - 1.0).abs() < 1e-5);
        }
        // Not unit length is fine: the score is about direction.
        let big: Vec<f32> = axis(2).iter().map(|x| x * 7.0).collect();
        assert_eq!(pv.score(&big).unwrap().top(), Category::Document);
        // A wrong size or a zero vector has no score.
        assert!(pv.score(&[1.0, 0.0]).is_none());
        assert!(pv.score(&[0.0; N]).is_none());
        assert!(pv.score(&[f32::NAN; N]).is_none());
    }

    #[test]
    fn a_category_scores_by_its_best_prompt() {
        // Two receipt prompts, one far away and one right on the photo.
        let mut per: Vec<Vec<Vec<f32>>> = (0..N).map(|i| vec![axis(i)]).collect();
        per[Category::Receipt.index()] = vec![axis(Category::Pet.index()), axis(1)];
        let pv = PromptVectors::from_vectors("m", per).unwrap();
        let zs = pv.score(&axis(Category::Receipt.index())).unwrap();
        assert!((zs.cosine(Category::Receipt) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn prompt_vectors_need_every_category() {
        assert!(PromptVectors::from_vectors("m", vec![]).is_err());
        let mut per: Vec<Vec<Vec<f32>>> = (0..N).map(|i| vec![axis(i)]).collect();
        per[3] = vec![];
        assert!(PromptVectors::from_vectors("m", per).is_err());
        let mut per: Vec<Vec<Vec<f32>>> = (0..N).map(|i| vec![axis(i)]).collect();
        per[3] = vec![vec![0.0; N]];
        assert!(PromptVectors::from_vectors("m", per).is_err());
        let pv = prompts();
        assert_eq!(pv.model(), "axes");
        assert_eq!(pv.dim(), N);
    }

    struct Words;
    impl Embedder for Words {
        fn model(&self) -> &str {
            "words"
        }
        fn dim(&self) -> usize {
            N
        }
        fn embed_images(&self, _: &[image::RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
            Err(MlError::Failed("no images".into()))
        }
        fn embed_text(&self, text: &str) -> Result<Vec<f32>, MlError> {
            Category::ALL
                .into_iter()
                .find(|c| c.prompts().contains(&text))
                .map(|c| axis(c.index()))
                .ok_or_else(|| MlError::Failed("unknown prompt".into()))
        }
        fn relevance(&self, cosine: f32) -> f32 {
            cosine
        }
    }

    #[test]
    fn prompt_vectors_are_built_from_the_embedders_text_tower() {
        let pv = PromptVectors::build(&Words).unwrap();
        assert_eq!(pv.model(), "words");
        assert_eq!(
            pv.score(&axis(Category::Meme.index())).unwrap().top(),
            Category::Meme
        );
        struct Broken;
        impl Embedder for Broken {
            fn model(&self) -> &str {
                "broken"
            }
            fn dim(&self) -> usize {
                4
            }
            fn embed_images(&self, _: &[image::RgbImage]) -> Result<Vec<Vec<f32>>, MlError> {
                Ok(vec![])
            }
            fn embed_text(&self, _: &str) -> Result<Vec<f32>, MlError> {
                Ok(vec![1.0])
            }
            fn relevance(&self, c: f32) -> f32 {
                c
            }
        }
        assert!(PromptVectors::build(&Broken).is_err());
    }

    #[test]
    fn name_hints_without_the_name() {
        use NameHint::*;
        let cases = [
            ("Screenshot_20240101-120000.png", Screenshot),
            ("Screenshot 2024-01-01 at 12.00.00.png", Screenshot),
            ("Screen Shot 2019-05-05 at 10.10.10 AM.png", Screenshot),
            ("Schermata 2024-01-01 alle 12.00.00.png", Screenshot),
            ("Skærmbillede 2024-01-01 kl. 12.00.00.png", Screenshot),
            ("Scan 12.jpeg", Scan),
            ("CamScanner 01-02-2024 10.11.jpg", Scan),
            ("Scansione 3.jpg", Scan),
            ("scan0001.jpg", Scan),
            ("IMG-20240101-WA0003.jpg", Chat),
            ("WhatsApp Image 2024-01-01 at 12.00.00.jpeg", Chat),
            ("photo_12@01-01-2024_12-00-00.jpg", Chat),
            ("received_123456.jpeg", Chat),
            ("IMG_1234.HEIC", Camera),
            ("DSC01234.JPG", Camera),
            ("PXL_20240101_120000123.jpg", Camera),
            ("20240101_120000.jpg", Camera),
            ("P1010001.JPG", Camera),
            ("Scandinavia holiday.jpg", Other),
            ("beach.jpg", Other),
            ("Beach day", Other),
            ("", Other),
        ];
        for (name, want) in cases {
            assert_eq!(NameHint::from_name(name), want, "{name:?}");
        }
    }

    #[test]
    fn a_feature_record_never_holds_the_name() {
        let zs = prompts().score(&axis(9)).unwrap();
        let mut fa = facts();
        fa.name = Some("IMG_8841_Maria-passport-copy.jpg");
        let f = Features::build(&fa, &zs);
        assert_eq!(f.name_hint, NameHint::Camera);
        let json = serde_json::to_string(&f).unwrap();
        for leak in ["maria", "passport", "8841", "IMG_", ".jpg"] {
            assert!(
                !json.to_lowercase().contains(&leak.to_lowercase()),
                "{leak} in {json}"
            );
        }
        let back: Features = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn screen_sizes_match_in_either_orientation() {
        assert!(is_screen_sized(1170, 2532));
        assert!(is_screen_sized(2532, 1170));
        assert!(is_screen_sized(1920, 1080));
        assert!(is_screen_sized(2560, 1440));
        assert!(is_screen_sized(1290, 2796));
        assert!(!is_screen_sized(4032, 3024));
        assert!(!is_screen_sized(1171, 2532));
        assert!(!is_screen_sized(0, 0));
        // Every listed size is stored short side first.
        assert!(SCREEN_SIZES.iter().all(|(s, l)| s < l));
    }

    #[test]
    fn the_record_holds_the_facts() {
        let f = feat(Category::Receipt, |fa| {
            fa.width = Some(1170);
            fa.height = Some(2532);
            fa.mime = "image/png";
            fa.camera = None;
            fa.name = Some("Screenshot 1.png");
            fa.has_gps = true;
            fa.source_kind = SourceKind::Chat;
        });
        assert_eq!(f.version, FEATURES_VERSION);
        assert_eq!((f.width, f.height), (1170, 2532));
        assert!((f.aspect - 0.462).abs() < 1e-3);
        assert!(f.screen_sized && !f.camera_exif && f.has_gps);
        assert_eq!(f.format, ImageFormat::Png);
        assert_eq!(f.name_hint, NameHint::Screenshot);
        assert_eq!(f.source_kind, SourceKind::Chat);
        assert_eq!(f.top_category, Category::Receipt);
        assert!(f.score(Category::Receipt) > 0.99);
        assert_eq!(f.zero_shot.len(), N);
        assert!(f.top_cosine > 0.99);
        // Unknown size, a blank camera string, a video.
        let u = feat(Category::Photo, |fa| {
            fa.width = None;
            fa.height = Some(-4);
            fa.camera = Some("  ");
            fa.kind = "video";
        });
        assert_eq!((u.width, u.height, u.aspect), (0, 0, 0.0));
        assert!(!u.screen_sized && !u.camera_exif);
        assert_eq!(u.kind, MediaKind::Video);
    }

    /// One row of the rules table: facts, what the picture looks like, the answer.
    struct Case {
        name: &'static str,
        look: Category,
        facts: fn(&mut Facts),
        want: Category,
        rule: &'static str,
    }

    fn screen(fa: &mut Facts) {
        fa.width = Some(1170);
        fa.height = Some(2532);
        fa.camera = None;
        fa.mime = "image/png";
        fa.name = None;
    }

    #[test]
    fn the_rules_table() {
        let rules = RulesDecider::default();
        let cases = [
            Case {
                name: "a video is left as photo",
                look: Category::Receipt,
                facts: |fa| fa.kind = "video",
                want: Category::Photo,
                rule: "not-a-photo",
            },
            Case {
                name: "screen-sized png without camera data",
                look: Category::Photo,
                facts: screen,
                want: Category::Screenshot,
                rule: "screenshot-screen-size",
            },
            Case {
                name: "screen-sized jpeg named screenshot",
                look: Category::Photo,
                facts: |fa| {
                    screen(fa);
                    fa.mime = "image/jpeg";
                    fa.name = Some("Screenshot_1.jpg");
                },
                want: Category::Screenshot,
                rule: "screenshot-screen-size",
            },
            Case {
                name: "screen-sized png from a camera stays a photo",
                look: Category::Photo,
                facts: |fa| {
                    screen(fa);
                    fa.camera = Some("Apple iPhone");
                },
                want: Category::Photo,
                rule: "default",
            },
            Case {
                name: "named screenshot of any size",
                look: Category::Photo,
                facts: |fa| {
                    fa.camera = None;
                    fa.name = Some("Screenshot_2.png");
                },
                want: Category::Screenshot,
                rule: "screenshot-name",
            },
            Case {
                name: "named screenshot with camera data is not trusted",
                look: Category::Photo,
                facts: |fa| fa.name = Some("Screenshot_2.png"),
                want: Category::Photo,
                rule: "default",
            },
            Case {
                name: "scanner app picks the document type it looks like",
                look: Category::Receipt,
                facts: |fa| {
                    fa.camera = None;
                    fa.name = Some("CamScanner 1.jpg");
                },
                want: Category::Receipt,
                rule: "scan-name",
            },
            Case {
                name: "scanner app with nothing clear is a document",
                look: Category::Pet,
                facts: |fa| fa.name = Some("Scan 7.jpg"),
                want: Category::Document,
                rule: "scan-name",
            },
            Case {
                name: "screen-sized jpeg that looks like a screenshot",
                look: Category::Screenshot,
                facts: |fa| {
                    screen(fa);
                    fa.mime = "image/jpeg";
                },
                want: Category::Screenshot,
                rule: "screenshot-lookalike",
            },
            Case {
                name: "screen-sized jpeg that looks like a landscape",
                look: Category::Photo,
                facts: |fa| {
                    screen(fa);
                    fa.mime = "image/jpeg";
                },
                want: Category::Photo,
                rule: "default",
            },
            Case {
                name: "photographed receipt",
                look: Category::Receipt,
                facts: |_| {},
                want: Category::Receipt,
                rule: "zero-shot",
            },
            Case {
                name: "photographed document with a position",
                look: Category::Document,
                facts: |fa| fa.has_gps = true,
                want: Category::Document,
                rule: "zero-shot",
            },
            Case {
                name: "food",
                look: Category::Food,
                facts: |_| {},
                want: Category::Food,
                rule: "zero-shot",
            },
            Case {
                name: "a pet photo with a position is still a pet",
                look: Category::Pet,
                facts: |fa| fa.has_gps = true,
                want: Category::Pet,
                rule: "zero-shot",
            },
            Case {
                name: "a screenshot-looking picture from a camera is not a screenshot",
                look: Category::Screenshot,
                facts: |_| {},
                want: Category::Photo,
                rule: "default",
            },
            Case {
                name: "a meme-looking camera picture is not a meme",
                look: Category::Meme,
                facts: |_| {},
                want: Category::Photo,
                rule: "default",
            },
            Case {
                name: "a meme from chat",
                look: Category::Meme,
                facts: |fa| {
                    fa.camera = None;
                    fa.source_kind = SourceKind::Chat;
                    fa.mime = "image/jpeg";
                    fa.width = Some(1080);
                    fa.height = Some(1080);
                },
                want: Category::Meme,
                rule: "zero-shot",
            },
            Case {
                name: "a map",
                look: Category::Map,
                facts: |fa| fa.camera = None,
                want: Category::Map,
                rule: "zero-shot",
            },
            Case {
                name: "a note",
                look: Category::Note,
                facts: |_| {},
                want: Category::Note,
                rule: "zero-shot",
            },
            Case {
                name: "a ticket",
                look: Category::Ticket,
                facts: |fa| fa.source_kind = SourceKind::Chat,
                want: Category::Ticket,
                rule: "zero-shot",
            },
            Case {
                name: "a plain photo",
                look: Category::Photo,
                facts: |_| {},
                want: Category::Photo,
                rule: "default",
            },
        ];
        for c in cases {
            let f = feat(c.look, c.facts);
            let d = rules.explain(&f);
            assert_eq!(
                (d.category, d.reason.as_str()),
                (c.want, c.rule),
                "{}",
                c.name
            );
            assert!((0.0..=1.0).contains(&d.confidence), "{}", c.name);
        }
    }

    #[test]
    fn confidence_follows_the_rule() {
        let rules = RulesDecider::default();
        assert_eq!(
            rules.explain(&feat(Category::Photo, screen)).confidence,
            0.95
        );
        let named = feat(Category::Photo, |fa| {
            fa.camera = None;
            fa.name = Some("Screenshot_3.png");
        });
        assert_eq!(rules.explain(&named).confidence, 0.9);
        let receipt = rules.explain(&feat(Category::Receipt, |_| {}));
        assert!(receipt.confidence > 0.99);
        let scan = rules.explain(&feat(Category::Pet, |fa| fa.name = Some("Scan 1.jpg")));
        assert_eq!(scan.confidence, 0.6);
    }

    /// Where a photo halfway between a category and `photo` flips, on synthetic vectors: the
    /// thresholds are tuned here, on made-up pictures, never on the owner's.
    #[test]
    fn the_zero_shot_threshold_flips_between_the_category_and_photo() {
        let rules = RulesDecider::default();
        let pv = prompts();
        let decide = |cat: Category, deg: f32, gps: bool| {
            let zs = pv.score(&mix(cat, Category::Photo, deg)).unwrap();
            let mut fa = facts();
            fa.has_gps = gps;
            rules.explain(&Features::build(&fa, &zs)).category
        };
        for cat in [Category::Receipt, Category::Document, Category::Ticket] {
            assert_eq!(decide(cat, 20.0, false), cat, "{cat:?} near");
            assert_eq!(decide(cat, 44.0, false), cat, "{cat:?} just on its side");
            assert_eq!(decide(cat, 45.0, false), Category::Photo, "{cat:?} tie");
            assert_eq!(decide(cat, 70.0, false), Category::Photo, "{cat:?} far");
        }
        // A tie with photo never wins, and the margin keeps a near tie out.
        assert_eq!(decide(Category::Pet, 44.9, false), Category::Photo);
        assert_eq!(decide(Category::Pet, 30.0, false), Category::Pet);
        // The GPS bump only matters for the categories that are not sensitive.
        let mut strict = feat(Category::Photo, |fa| fa.has_gps = true);
        strict.zero_shot.insert(Category::Pet, 0.52);
        strict.zero_shot.insert(Category::Photo, 0.30);
        assert_eq!(rules.explain(&strict).category, Category::Photo);
        strict.has_gps = false;
        assert_eq!(rules.explain(&strict).category, Category::Pet);
        let mut sens = feat(Category::Photo, |fa| fa.has_gps = true);
        sens.zero_shot.insert(Category::Receipt, 0.52);
        sens.zero_shot.insert(Category::Photo, 0.30);
        assert_eq!(rules.explain(&sens).category, Category::Receipt);
    }

    #[test]
    fn the_best_passing_category_wins() {
        let rules = RulesDecider::default();
        let mut f = feat(Category::Photo, |_| {});
        for (c, s) in [
            (Category::Receipt, 0.45),
            (Category::Document, 0.5),
            (Category::Photo, 0.05),
        ] {
            f.zero_shot.insert(c, s);
        }
        let d = rules.explain(&f);
        assert_eq!(d.category, Category::Document);
        assert_eq!(d.confidence, 0.5);
        // With nothing passing the confidence says how little else there is.
        let mut g = feat(Category::Photo, |_| {});
        for c in Category::ALL {
            g.zero_shot.insert(c, 0.0);
        }
        g.zero_shot.insert(Category::Photo, 0.2);
        g.zero_shot.insert(Category::Pet, 0.3);
        let d = rules.explain(&g);
        assert_eq!(d.category, Category::Photo);
        assert_eq!(d.confidence, 0.7);
    }

    /// A second decider slots in through the trait: here one that always answers "meme".
    #[test]
    fn another_decider_implements_the_same_trait() {
        struct Always(Category);
        impl Decider for Always {
            fn kind(&self) -> DeciderKind {
                DeciderKind::Jev
            }
            fn id(&self) -> String {
                "jev-test".into()
            }
            fn decide(&self, _: &Features) -> Result<Decision, MlError> {
                Ok(Decision {
                    category: self.0,
                    confidence: 0.9,
                    reason: "test".into(),
                })
            }
        }
        let deciders: Vec<Box<dyn Decider>> = vec![
            Box::new(RulesDecider::default()),
            Box::new(Always(Category::Meme)),
        ];
        let f = feat(Category::Photo, |_| {});
        let got: Vec<(&str, String, Category)> = deciders
            .iter()
            .map(|d| (d.kind().as_str(), d.id(), d.decide(&f).unwrap().category))
            .collect();
        assert_eq!(
            got,
            [
                ("rules", "rules-1".to_string(), Category::Photo),
                ("jev", "jev-test".to_string(), Category::Meme)
            ]
        );
    }
}
