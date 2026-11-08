//! Plans: what a request will do at Meshy and what it costs, **before** any
//! credit is spent. A plan is a list of steps (each a Meshy task), the
//! animation clips chosen (ids, in-place variants), the export formats and
//! the total in credits. The owner confirms the plan; the runner executes
//! exactly the plan's [`Spec`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::anim::{self, Clip};
use crate::api::TaskKind;
use crate::pricing::{self, TextureRes};

/// Tool actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Plan,
    TextTo3d,
    ImageTo3d,
    Remesh,
    Texture,
    Rig,
    Animate,
    Export,
    Status,
    List,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        Some(
            match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
                "plan" => Action::Plan,
                "text_to_3d" | "text" => Action::TextTo3d,
                "image_to_3d" | "image" => Action::ImageTo3d,
                "remesh" => Action::Remesh,
                "texture" | "retexture" => Action::Texture,
                "rig" => Action::Rig,
                "animate" => Action::Animate,
                "export" | "convert" => Action::Export,
                "status" => Action::Status,
                "list" | "gallery" => Action::List,
                _ => return None,
            },
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Action::Plan => "plan",
            Action::TextTo3d => "text_to_3d",
            Action::ImageTo3d => "image_to_3d",
            Action::Remesh => "remesh",
            Action::Texture => "texture",
            Action::Rig => "rig",
            Action::Animate => "animate",
            Action::Export => "export",
            Action::Status => "status",
            Action::List => "list",
        }
    }

    /// Spends credits (needs the owner's confirmation).
    pub fn spends(self) -> bool {
        !matches!(self, Action::Plan | Action::Status | Action::List)
    }

    /// What MCP callers (ChatGPT, Grok, Claude) may use.
    pub fn mcp_allowed(self) -> bool {
        matches!(self, Action::Plan | Action::List)
    }
}

/// The `meshy` tool's arguments.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Args {
    /// plan (cost estimate, spends nothing), text_to_3d, image_to_3d, remesh, texture, rig,
    /// animate, export, status (a job), list (the gallery). Everything except plan, status
    /// and list spends Meshy credits and needs Giuseppe's explicit yes.
    pub action: String,
    /// With action=plan: the action to estimate (default text_to_3d).
    pub for_action: Option<String>,
    /// What to make (text_to_3d), in English works best; or the new look (texture).
    pub prompt: Option<String>,
    /// image_to_3d: one image (http(s) URL, data: URI, or "upload:<file>" from the owner's uploads).
    pub image_url: Option<String>,
    /// image_to_3d: 2-4 views of the same object instead of image_url.
    pub image_urls: Option<Vec<String>>,
    /// remesh/texture/rig/animate/export: a gallery model id (mdl_…) to work on.
    pub model_id: Option<String>,
    /// Advanced: Meshy model, one of latest (default), meshy-7.1, meshy-6, meshy-6-lite, meshy-t2, meshy-t2.5.
    pub ai_model: Option<String>,
    /// Clean low-poly game asset (Smart Topology, meshy-t2.5): cheaper preview (5 credits).
    pub low_poly: Option<bool>,
    /// Target polygon count (100-300000; low-poly: up to 30000).
    pub target_polycount: Option<u32>,
    /// quad or triangle (default triangle; quad not with low_poly).
    pub topology: Option<String>,
    /// Texture the model (default true).
    pub texture: Option<bool>,
    /// Texture style, e.g. "weathered oak with brass fittings".
    pub texture_prompt: Option<String>,
    /// 2k (default), 4k or 8k (8k costs more).
    pub texture_resolution: Option<String>,
    /// PBR maps (metallic, roughness, normal).
    pub pbr: Option<bool>,
    /// Auto-rig a humanoid character (includes free walk and run clips).
    pub rig: Option<bool>,
    /// Character height in meters for rigging (default 1.7).
    pub height_m: Option<f64>,
    /// Animation clips: names ("walk", "wave", "Formal Bow") or library ids ("613"). Implies rig.
    pub animations: Option<Vec<String>>,
    /// Prefer in-place variants of walks and runs (default true).
    pub in_place: Option<bool>,
    /// Extra formats: glb (always), fbx, usdz, obj, stl (3D printing), 3mf.
    pub formats: Option<Vec<String>>,
    /// A short title for the gallery.
    pub title: Option<String>,
    /// status: the job id.
    pub job_id: Option<String>,
    /// list: how many (default 10).
    pub limit: Option<u32>,
}

impl Args {
    pub fn from_value(v: &Value) -> Result<Self, String> {
        serde_json::from_value(v.clone()).map_err(|e| format!("invalid arguments: {e}"))
    }

    pub fn action(&self) -> Result<Action, String> {
        Action::parse(&self.action).ok_or_else(|| {
            "action: one of plan, text_to_3d, image_to_3d, remesh, texture, rig, animate, export, status, list".into()
        })
    }

    /// The action a plan is for (`plan` → `for_action`, default text_to_3d).
    pub fn planned_action(&self) -> Result<Action, String> {
        let a = self.action()?;
        if a != Action::Plan {
            return Ok(a);
        }
        let f = self.for_action.as_deref().unwrap_or("text_to_3d");
        let p = Action::parse(f).ok_or_else(|| format!("for_action: unknown action {f}"))?;
        if !p.spends() {
            return Err("for_action: a spending action (text_to_3d, image_to_3d, remesh, texture, rig, animate, export)".into());
        }
        Ok(p)
    }
}

/// The task a post-processing action works on (from the gallery).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRef {
    pub model_id: Option<String>,
    pub task_id: String,
    pub kind: TaskKind,
    /// The rigging task, when the model is rigged (animate needs it).
    pub rig_task_id: Option<String>,
    pub title: Option<String>,
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Preview,
    Refine,
    ImageTo3d,
    MultiImageTo3d,
    Remesh,
    Retexture,
    Rig,
    Animate,
    Convert,
}

impl StepKind {
    pub fn task_kind(self) -> TaskKind {
        match self {
            StepKind::Preview | StepKind::Refine => TaskKind::TextTo3d,
            StepKind::ImageTo3d => TaskKind::ImageTo3d,
            StepKind::MultiImageTo3d => TaskKind::MultiImageTo3d,
            StepKind::Remesh => TaskKind::Remesh,
            StepKind::Retexture => TaskKind::Retexture,
            StepKind::Rig => TaskKind::Rigging,
            StepKind::Animate => TaskKind::Animation,
            StepKind::Convert => TaskKind::Convert,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Preview => "preview",
            StepKind::Refine => "refine",
            StepKind::ImageTo3d => "image_to_3d",
            StepKind::MultiImageTo3d => "multi_image_to_3d",
            StepKind::Remesh => "remesh",
            StepKind::Retexture => "texture",
            StepKind::Rig => "rig",
            StepKind::Animate => "animate",
            StepKind::Convert => "export",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub kind: StepKind,
    pub credits: u32,
    pub detail: String,
}

/// Where the model comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    Text { prompt: String },
    Images { urls: Vec<String> },
    Input(InputRef),
}

/// Everything the runner needs, normalized and validated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    pub action: Action,
    pub source: Source,
    pub ai_model: String,
    pub smart_topology: bool,
    pub topology: Option<String>,
    pub target_polycount: Option<u32>,
    pub texture: Option<TextureRes>,
    pub texture_prompt: Option<String>,
    pub pbr: bool,
    pub rig: bool,
    pub height_m: f64,
    pub clips: Vec<Clip>,
    pub formats: Vec<String>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub spec: Spec,
    pub steps: Vec<Step>,
    pub total: u32,
    pub notes: Vec<String>,
}

pub const FORMATS: &[&str] = &["glb", "fbx", "usdz", "obj", "stl", "3mf"];
const MAX_PROMPT: usize = 800;

fn clean_text(s: Option<&str>, max: usize) -> Option<String> {
    let t: String = s?
        .chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect::<String>()
        .trim()
        .to_owned();
    if t.is_empty() {
        None
    } else {
        Some(t.chars().take(max).collect())
    }
}

fn title_from(prompt: &str) -> String {
    let words: Vec<&str> = prompt.split_whitespace().take(6).collect();
    let t = words.join(" ");
    let t = t.trim_end_matches(|c: char| !c.is_alphanumeric());
    let mut c = t.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => "3D model".into(),
    }
}

/// Image references the runner can send: http(s), data:image/…, upload:<name>.
pub fn valid_image_ref(s: &str) -> bool {
    let t = s.trim();
    let lower = t.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return !t.chars().any(char::is_whitespace) && t.len() <= 2048;
    }
    if lower.starts_with("data:image/") {
        return t.len() <= 14 * 1024 * 1024;
    }
    if let Some(name) = t.strip_prefix("upload:") {
        return !name.is_empty()
            && name.len() <= 128
            && !name.contains("..")
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    }
    false
}

/// Build the plan. `input` is required for remesh/texture/rig/animate/export
/// (the caller resolves `model_id` through the gallery).
pub fn build(args: &Args, input: Option<&InputRef>) -> Result<Plan, String> {
    let action = args.planned_action()?;
    let mut notes = Vec::new();
    let low_poly = args.low_poly.unwrap_or(false);
    let ai_model = match args
        .ai_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(m) if pricing::MODELS.contains(&m) => m.to_owned(),
        Some(m) => return Err(format!("ai_model: unknown Meshy model {m}")),
        None if low_poly => "meshy-t2.5".into(),
        None => "latest".into(),
    };
    let smart_topology = pricing::is_smart_topology(&ai_model);
    let topology = match args
        .topology
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t @ ("quad" | "triangle")) => Some(t.to_owned()),
        Some(t) => return Err(format!("topology: quad or triangle, not {t}")),
        None => None,
    };
    if smart_topology && topology.as_deref() == Some("quad") {
        return Err("topology: quad is not available with low-poly (Smart Topology) models".into());
    }
    let max_poly = if smart_topology {
        if ai_model == "meshy-t2" {
            15_000
        } else {
            30_000
        }
    } else {
        300_000
    };
    if let Some(p) = args.target_polycount {
        if !(100..=max_poly).contains(&p) {
            return Err(format!(
                "target_polycount: 100 to {max_poly} for {ai_model}"
            ));
        }
    }
    let texture_on = args.texture.unwrap_or(true);
    let res = match args.texture_resolution.as_deref() {
        Some(s) => TextureRes::parse(s).ok_or("texture_resolution: 2k, 4k or 8k")?,
        None => TextureRes::K2,
    };
    let texture_prompt = clean_text(args.texture_prompt.as_deref(), MAX_PROMPT);
    let in_place = args.in_place.unwrap_or(true);
    let mut clips: Vec<Clip> = Vec::new();
    for a in args.animations.iter().flatten() {
        let c = anim::resolve(a, in_place)?;
        if !clips.iter().any(|x| x.id == c.id) {
            clips.push(c);
        }
    }
    if clips.len() > pricing::MAX_ACTIONS_PER_REQUEST {
        return Err(format!(
            "animations: at most {} clips per request",
            pricing::MAX_ACTIONS_PER_REQUEST
        ));
    }
    let mut formats = vec!["glb".to_owned()];
    for f in args.formats.iter().flatten() {
        let f = f.trim().trim_start_matches('.').to_ascii_lowercase();
        if !FORMATS.contains(&f.as_str()) {
            return Err(format!("formats: one of {}", FORMATS.join(", ")));
        }
        if !formats.contains(&f) {
            formats.push(f);
        }
    }
    let rig = args.rig.unwrap_or(false) || !clips.is_empty();
    let height_m = args.height_m.unwrap_or(1.7);
    if !(0.1..=20.0).contains(&height_m) {
        return Err("height_m: 0.1 to 20 meters".into());
    }
    let prompt = clean_text(args.prompt.as_deref(), MAX_PROMPT);
    let mut steps = Vec::new();
    let poly_note = || {
        let t = topology.as_deref().unwrap_or("triangle");
        match args.target_polycount {
            Some(p) => format!("{t} mesh, ~{p} faces"),
            None => format!("{t} mesh"),
        }
    };
    let extra_formats: Vec<&String> = formats.iter().filter(|f| *f != "glb").collect();
    let source = match action {
        Action::TextTo3d => {
            let p = prompt.clone().ok_or("prompt: describe the model to make")?;
            steps.push(Step {
                kind: StepKind::Preview,
                credits: pricing::text_preview(&ai_model, false),
                detail: format!("untextured {} ({ai_model})", poly_note()),
            });
            if texture_on {
                steps.push(Step {
                    kind: StepKind::Refine,
                    credits: pricing::refine(res),
                    detail: format!(
                        "textures {}{}{}",
                        res.as_str(),
                        if args.pbr.unwrap_or(false) {
                            ", PBR"
                        } else {
                            ""
                        },
                        texture_prompt
                            .as_deref()
                            .map(|t| format!(": {t}"))
                            .unwrap_or_default()
                    ),
                });
            } else {
                notes.push("No textures: the model stays a grey mesh.".into());
            }
            Source::Text { prompt: p }
        }
        Action::ImageTo3d => {
            let mut urls: Vec<String> = Vec::new();
            if let Some(u) = args.image_url.as_deref().filter(|u| !u.trim().is_empty()) {
                urls.push(u.trim().to_owned());
            }
            for u in args.image_urls.iter().flatten() {
                if !u.trim().is_empty() && !urls.contains(&u.trim().to_owned()) {
                    urls.push(u.trim().to_owned());
                }
            }
            if urls.is_empty() || urls.len() > 4 {
                return Err("image_url (or 1-4 image_urls) is required".into());
            }
            if let Some(bad) = urls.iter().find(|u| !valid_image_ref(u)) {
                let shown: String = bad.chars().take(40).collect();
                return Err(format!(
                    "image: not an http(s) URL, data:image URI or upload:<file>: {shown}"
                ));
            }
            let multi = urls.len() > 1;
            if multi && smart_topology {
                return Err("several images need meshy-7.1, meshy-6 or meshy-6-lite".into());
            }
            let credits = pricing::image_to_3d(&ai_model, texture_on.then_some(res), false)
                .ok_or_else(|| format!("{ai_model} does not support {} textures", res.as_str()))?;
            steps.push(Step {
                kind: if multi {
                    StepKind::MultiImageTo3d
                } else {
                    StepKind::ImageTo3d
                },
                credits,
                detail: format!(
                    "{} image{} → {}{} ({ai_model})",
                    urls.len(),
                    if multi { "s" } else { "" },
                    poly_note(),
                    if texture_on {
                        format!(", textures {}", res.as_str())
                    } else {
                        ", no textures".into()
                    }
                ),
            });
            Source::Images { urls }
        }
        Action::Remesh | Action::Texture | Action::Rig | Action::Animate | Action::Export => {
            let input = input.ok_or("model_id: which gallery model (mdl_…) to work on")?;
            match action {
                Action::Remesh => steps.push(Step {
                    kind: StepKind::Remesh,
                    credits: pricing::REMESH,
                    detail: poly_note(),
                }),
                Action::Texture => {
                    let p = prompt
                        .clone()
                        .or(texture_prompt.clone())
                        .ok_or("prompt: the new look")?;
                    steps.push(Step {
                        kind: StepKind::Retexture,
                        credits: pricing::retexture(res),
                        detail: format!("new textures {}: {p}", res.as_str()),
                    });
                }
                Action::Export => {
                    if extra_formats.is_empty() {
                        return Err(
                            "formats: which format to export (fbx, usdz, obj, stl, 3mf)".into()
                        );
                    }
                    steps.push(Step {
                        kind: StepKind::Convert,
                        credits: pricing::CONVERT,
                        detail: format!(
                            "convert to {} (free when Meshy already made it)",
                            extra_formats
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    });
                }
                Action::Animate if clips.is_empty() => {
                    return Err("animations: which clips (names or ids)".into());
                }
                _ => {}
            }
            Source::Input(input.clone())
        }
        Action::Plan | Action::Status | Action::List => {
            unreachable!("planned_action is a spending action")
        }
    };
    let already_rigged = matches!(&source, Source::Input(i) if i.rig_task_id.is_some());
    let wants_rig = rig || action == Action::Rig;
    let rig_step = match action {
        Action::Rig => true,
        Action::Animate => !already_rigged,
        _ => wants_rig && !already_rigged,
    };
    if rig_step {
        if !texture_on && matches!(action, Action::TextTo3d) {
            notes.push("Rigging works best on a textured humanoid.".into());
        }
        steps.push(Step {
            kind: StepKind::Rig,
            credits: pricing::RIG,
            detail: format!(
                "auto-rig humanoid, {height_m} m (free Walking and Running clips included)"
            ),
        });
    }
    if !clips.is_empty() {
        steps.push(Step {
            kind: StepKind::Animate,
            credits: pricing::ANIMATION_PER_ACTION * clips.len() as u32,
            detail: format!(
                "{} clip{}: {}",
                clips.len(),
                if clips.len() == 1 { "" } else { "s" },
                clips.iter().map(Clip::label).collect::<Vec<_>>().join(", ")
            ),
        });
    }
    if matches!(action, Action::TextTo3d | Action::ImageTo3d) && !extra_formats.is_empty() {
        notes.push(format!(
            "{} made at generation time (no extra credits).",
            extra_formats
                .iter()
                .map(|s| s.to_uppercase())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if rig && matches!(action, Action::TextTo3d | Action::ImageTo3d) {
        notes.push("Rigging needs a humanoid (two arms, two legs); other shapes may fail and the rig credits are refunded.".into());
    }
    let title = clean_text(args.title.as_deref(), 80)
        .or_else(|| prompt.as_deref().map(title_from))
        .or_else(|| match &source {
            Source::Input(i) => i.title.clone(),
            _ => None,
        })
        .unwrap_or_else(|| "3D model".into());
    let total = steps.iter().map(|s| s.credits).sum();
    Ok(Plan {
        spec: Spec {
            action,
            source,
            ai_model,
            smart_topology,
            topology,
            target_polycount: args.target_polycount,
            texture: texture_on.then_some(res),
            texture_prompt,
            pbr: args.pbr.unwrap_or(false),
            rig: wants_rig,
            height_m,
            clips,
            formats,
            title,
        },
        steps,
        total,
        notes,
    })
}

impl Plan {
    /// One line for confirmations and the timeline:
    /// `meshy text_to_3d "Treasure chest": preview 20 + refine 10 = 30 credits`.
    pub fn summary(&self) -> String {
        let steps: Vec<String> = self
            .steps
            .iter()
            .map(|s| format!("{} {}", s.kind.as_str(), s.credits))
            .collect();
        format!(
            "meshy {} \"{}\": {} = {} credits",
            self.spec.action.as_str(),
            self.spec.title,
            steps.join(" + "),
            self.total
        )
    }

    /// What the budget reserves for this plan: every step's worst case
    /// (`budget::worst_case`), not the estimate (verification G4).
    pub fn reservation(&self) -> u32 {
        self.steps
            .iter()
            .map(|s| crate::budget::worst_case(s.credits))
            .fold(0u32, u32::saturating_add)
    }

    /// For the model and the app.
    pub fn to_json(&self) -> Value {
        json!({
            "action": self.spec.action.as_str(),
            "title": self.spec.title,
            "steps": self.steps.iter().map(|s| json!({"step": s.kind.as_str(), "credits": s.credits, "detail": s.detail})).collect::<Vec<_>>(),
            "clips": self.spec.clips.iter().map(|c| json!({"actionId": c.id, "name": c.name, "inPlace": c.in_place})).collect::<Vec<_>>(),
            "formats": self.spec.formats,
            "totalCredits": self.total,
            "reservedCredits": self.reservation(),
            "notes": self.notes,
            "pricesAsOf": pricing::PRICES_AS_OF,
        })
    }

    /// The plan as text (Telegram, plain-text fallback).
    pub fn text(&self) -> String {
        let mut out = format!("🧊 {} — {} credits\n", self.spec.title, self.total);
        for (i, s) in self.steps.iter().enumerate() {
            out.push_str(&format!(
                "{}. {} ({}): {}\n",
                i + 1,
                s.kind.as_str(),
                s.credits,
                s.detail
            ));
        }
        for n in &self.notes {
            out.push_str(&format!("• {n}\n"));
        }
        out.trim_end().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: Value) -> Args {
        Args::from_value(&v).unwrap()
    }

    #[test]
    fn text_to_3d_character_plan() {
        let p = build(
            &args(
                json!({"action": "text_to_3d", "prompt": "a knight in silver armor",
                         "animations": ["walk", "wave"], "formats": ["fbx", "usdz"]}),
            ),
            None,
        )
        .unwrap();
        let kinds: Vec<_> = p.steps.iter().map(|s| (s.kind, s.credits)).collect();
        assert_eq!(
            kinds,
            vec![
                (StepKind::Preview, 20),
                (StepKind::Refine, 10),
                (StepKind::Rig, 5),
                (StepKind::Animate, 6)
            ]
        );
        assert_eq!(p.total, 41);
        assert_eq!(p.spec.clips[0].id, 613, "in-place casual walk");
        assert_eq!(p.spec.clips[1].id, 290);
        assert_eq!(p.spec.formats, vec!["glb", "fbx", "usdz"]);
        assert!(p.summary().contains("= 41 credits"), "{}", p.summary());
        assert!(p.text().contains("Casual Walk (in place, #613)"));
        assert_eq!(p.spec.title, "A knight in silver armor");
    }

    #[test]
    fn low_poly_and_validation() {
        let p = build(
            &args(
                json!({"action": "plan", "for_action": "text_to_3d", "prompt": "treasure chest",
                         "low_poly": true, "target_polycount": 3000, "texture": false}),
            ),
            None,
        )
        .unwrap();
        assert_eq!((p.total, p.spec.ai_model.as_str()), (5, "meshy-t2.5"));
        for bad in [
            json!({"action": "text_to_3d"}),
            json!({"action": "text_to_3d", "prompt": "x", "low_poly": true, "topology": "quad"}),
            json!({"action": "text_to_3d", "prompt": "x", "target_polycount": 50}),
            json!({"action": "text_to_3d", "prompt": "x", "formats": ["exe"]}),
            json!({"action": "image_to_3d", "image_url": "file:///etc/passwd"}),
            json!({"action": "image_to_3d", "image_url": "upload:../x.png"}),
            json!({"action": "rig"}),
            json!({"action": "plan", "for_action": "list"}),
            json!({"action": "explode"}),
        ] {
            let a = Args::from_value(&bad).unwrap();
            assert!(build(&a, None).is_err(), "{bad}");
        }
    }

    #[test]
    fn post_processing_plans() {
        let input = InputRef {
            model_id: Some("mdl_abc".into()),
            task_id: "t1".into(),
            kind: TaskKind::TextTo3d,
            rig_task_id: Some("r1".into()),
            title: Some("Knight".into()),
            prompt: None,
        };
        let anim = build(
            &args(json!({"action": "animate", "animations": ["dance", "bow"]})),
            Some(&input),
        )
        .unwrap();
        assert_eq!(anim.total, 6, "already rigged: no rig step");
        let unrigged = InputRef {
            rig_task_id: None,
            ..input.clone()
        };
        let anim = build(
            &args(json!({"action": "animate", "animations": ["dance"]})),
            Some(&unrigged),
        )
        .unwrap();
        assert_eq!(anim.total, 8, "rig 5 + 1 clip");
        let rm = build(
            &args(json!({"action": "remesh", "target_polycount": 5000, "topology": "quad"})),
            Some(&input),
        )
        .unwrap();
        assert_eq!(rm.total, 5);
        let ex = build(
            &args(json!({"action": "export", "formats": ["stl"]})),
            Some(&input),
        )
        .unwrap();
        assert_eq!(ex.total, 1);
        let tx = build(
            &args(json!({"action": "texture", "prompt": "gold", "texture_resolution": "8k"})),
            Some(&input),
        )
        .unwrap();
        assert_eq!(tx.total, 15);
        let img = build(&args(json!({"action": "image_to_3d", "image_url": "https://example.com/mug.jpg", "formats": ["stl"]})), None).unwrap();
        assert_eq!(img.total, 30);
        assert_eq!(img.spec.title, "3D model");
        assert!(Action::Plan.mcp_allowed() && Action::List.mcp_allowed());
        assert!(!Action::Status.mcp_allowed() && !Action::TextTo3d.mcp_allowed());
    }
}
