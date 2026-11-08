//! The JEV decider: TypeSafe's Jev, a typed decision model (`lucina-jev`), picks a photo's
//! category from its feature record.
//!
//! Jev takes no pictures, so it gets the record as a small JSON state of labels and booleans
//! (Jev is weak with raw numbers): the categories the picture looks most like, in order, with a
//! strong / some / weak level each; screen-sized; camera data; file format; where it came from;
//! what its file name suggests (a hint, never the name); has a position (a yes, never the place).
//! One choice question over the ten categories with a description each; the answer and its
//! confidence become the [`Decision`].
//!
//! Errors: an outage (quota, credits, overload, cooling down, timeout, network, no key) is
//! [`MlError::Unavailable`], so the photo stays pending and the rules stand in for the photos with
//! no decision yet; anything else (a malformed answer, a refused request) is [`MlError::Failed`].
//! [`Decider::ready`] is the client's `available()`.
//!
//! The client is async; `decide` runs it on a private single-thread runtime, on its own short-lived
//! thread, so it works from the embedding thread and from a request's blocking task alike.

use lucina_jev::{Jev, JevError, Question};
use serde_json::{json, Map, Value};

use crate::categories::{
    Category, Decider, DeciderKind, Decision, Features, ImageFormat, NameHint, SourceKind,
};
use crate::MlError;

/// Bumped when the state or the question changes: every photo is decided again.
pub const QUESTION_VERSION: u32 = 1;
/// The caller name in the client's logs and stats.
pub const PURPOSE: &str = "gallery-category";
/// Categories named in the state, the closest first.
const TOP: usize = 3;

/// What each category means, as the question's options.
pub fn option_text(c: Category) -> &'static str {
    match c {
        Category::Screenshot => "a screenshot of a phone, tablet or computer screen",
        Category::Receipt => "a receipt, bill or invoice",
        Category::Document => "a document, form, letter, contract or ID card or passport",
        Category::Note => "handwritten notes, a whiteboard or lecture slides",
        Category::Ticket => "a ticket, boarding pass or QR code to show",
        Category::Map => "a map or directions",
        Category::Meme => "a meme or a picture saved from chats or the internet",
        Category::Food => "a photo of food or a meal",
        Category::Pet => "a photo of a pet or another animal",
        Category::Photo => "an ordinary photo (people, places, things, anything else)",
    }
}

fn level(score: f32) -> &'static str {
    if score >= 0.5 {
        "strong"
    } else if score >= 0.2 {
        "some"
    } else {
        "weak"
    }
}

fn format_label(f: ImageFormat) -> &'static str {
    match f {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpeg",
        ImageFormat::Heic => "heic",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Other => "other",
    }
}

fn source_label(s: SourceKind) -> &'static str {
    match s {
        SourceKind::PhoneBackup => "phone camera roll backup",
        SourceKind::Chat => "received in a chat or shared",
        SourceKind::Lucina => "made or saved by the assistant",
        SourceKind::DriveFolder => "imported from a folder",
        SourceKind::Other => "unknown",
    }
}

fn name_label(n: NameHint) -> &'static str {
    match n {
        NameHint::Screenshot => "screenshot",
        NameHint::Scan => "scanner app",
        NameHint::Chat => "chat app",
        NameHint::Camera => "camera default name",
        NameHint::Other => "nothing in particular",
    }
}

/// The state Jev sees for one photo: labels and booleans only, no names, no places, no pixels.
pub fn state_of(f: &Features) -> Value {
    let mut ranked: Vec<(Category, f32)> = f.zero_shot.iter().map(|(c, s)| (*c, *s)).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut looks = Map::new();
    for (c, s) in ranked.iter().take(TOP) {
        looks.insert(c.as_str().to_string(), json!(level(*s)));
    }
    json!({
        "looks_most_like": ranked.iter().take(TOP).map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
        "likeness": looks,
        "screen_sized": f.screen_sized,
        "camera_metadata": f.camera_exif,
        "file_format": format_label(f.format),
        "came_from": source_label(f.source_kind),
        "file_name_suggests": name_label(f.name_hint),
        "has_location": f.has_gps,
        "portrait": f.height > f.width && f.width > 0,
    })
}

pub fn question() -> Question {
    Question::choice(
        "Which kind of picture is this, judging from what an image model says it looks like and \
         from its file facts? A real camera photo with camera metadata and a location is most \
         likely an ordinary photo, food or a pet.",
        Category::ALL.iter().map(|c| (c.as_str(), option_text(*c))),
    )
}

/// An outage (try later) or a failure for this photo.
pub fn map_error(e: JevError) -> MlError {
    match e {
        JevError::NotConfigured
        | JevError::CoolingDown(_)
        | JevError::Quota(_)
        | JevError::Credits(_)
        | JevError::Overloaded(_)
        | JevError::Timeout
        | JevError::Transport(_) => MlError::Unavailable(e.to_string()),
        JevError::BadRequest { .. } | JevError::Invalid(_) | JevError::Malformed(_) => {
            MlError::Failed(e.to_string())
        }
    }
}

pub struct JevDecider {
    jev: &'static Jev,
}

impl JevDecider {
    pub fn new(jev: &'static Jev) -> JevDecider {
        JevDecider { jev }
    }

    /// The process-wide client from the environment (`JEV_API_KEY`, `LUCINA_JEV`…), when it has a
    /// key and is enabled.
    pub fn from_env() -> Option<JevDecider> {
        let jev = lucina_jev::shared();
        jev.configured().then(|| JevDecider::new(jev))
    }

    fn ask(&self, state: Value) -> Result<lucina_jev::Decision, JevError> {
        let jev = self.jev;
        let q = [("category", question())];
        std::thread::scope(|s| {
            s.spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| JevError::Transport(e.to_string()))?;
                rt.block_on(jev.decide(PURPOSE, state, &q))
            })
            .join()
            .unwrap_or_else(|_| Err(JevError::Transport("the JEV call panicked".into())))
        })
    }
}

impl Decider for JevDecider {
    fn kind(&self) -> DeciderKind {
        DeciderKind::Jev
    }

    fn id(&self) -> String {
        format!("jev:{}:q{QUESTION_VERSION}", self.jev.model())
    }

    fn ready(&self) -> bool {
        self.jev.available()
    }

    fn decide(&self, f: &Features) -> Result<Decision, MlError> {
        let d = self.ask(state_of(f)).map_err(map_error)?;
        let (name, confidence) = d
            .choice("category")
            .ok_or_else(|| MlError::Failed("JEV gave no category".into()))?;
        let category = Category::parse(name)
            .ok_or_else(|| MlError::Failed(format!("JEV answered an unknown category {name}")))?;
        Ok(Decision {
            category,
            confidence: confidence as f32,
            reason: format!("jev {confidence:.2}"),
        })
    }
}
