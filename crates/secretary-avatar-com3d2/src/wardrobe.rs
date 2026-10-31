//! Private modular wardrobe library. Menus remain inert data. Native material
//! overrides, masks and imported legacy garments are composed in Rust on demand.
use crate::{
    animation::Animation,
    arc::Archive,
    bindings, catalog, compose, glb_io, gltf, invalid, legacy, masking, mate,
    menu::Menu,
    model::Model,
    pose_asset,
    resources::{ArchiveSet, ResourceSource},
    Result,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
const VERSION: u32 = 3;
const PIPELINE_REVISION: u32 = 30;
/// Categories never offered for selection: swimwear, and underwear on its own (the coverage
/// guard below still uses plain inner wear).
const HIDDEN_CATEGORIES: &[&str] = &["mizugi", "bra", "panz"];
/// Name fragments of adult or fetish items in the game data (skirt flips, slipping towels and
/// swimsuits, bondage, toys, blindfolds, pet collars, "love nurse"), never offered.
const ADULT_NAME_PARTS: &[&str] = &[
    "めくれ", "ずらし", "タオル", "ボンデージ", "サキュバス", "バイブ", "Ｃｈｕ-Ｂ", "ＳＭ", "SM用", "目隠し",
    "アイマスク", "首輪", "ラブ・ナース", "注射器", "水着", "バニー", "カルテ",
];
/// Whether an item may be offered: not a hidden category, not an adult item.
fn safe_item(name: &str, category: &str) -> bool {
    !HIDDEN_CATEGORIES.contains(&category) && !ADULT_NAME_PARTS.iter().any(|p| name.contains(p))
}
/// What a look covers: (chest, hips). A complete imported outfit covers both.
fn coverage<'a>(categories: impl IntoIterator<Item = &'a str>) -> (bool, bool) {
    let (mut top, mut bottom) = (false, false);
    for c in categories {
        top |= matches!(c, "wear" | "onepiece" | "original" | "bra");
        bottom |= matches!(c, "skirt" | "onepiece" | "original" | "panz");
    }
    (top, bottom)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Source {
    Native {
        menu: String,
    },
    Legacy {
        path: String,
        hair: bool,
        clothes: bool,
        #[serde(default)]
        fit: Option<legacy::FitProfile>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
struct Item {
    id: String,
    name: String,
    category: String,
    origin: String,
    source: Source,
}
#[derive(Clone, Serialize, Deserialize)]
struct Preset {
    id: String,
    name: String,
    items: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Config {
    version: u32,
    canonical: String,
    model: String,
    animation: String,
    archives: Vec<String>,
    #[serde(default)]
    loose_surface_items: Vec<String>,
    #[serde(default)]
    loose_component_sizes: std::collections::BTreeMap<String, Vec<usize>>,
    #[serde(default)]
    deferred_origins: Vec<String>,
    #[serde(default)]
    deferred_items: Vec<String>,
    items: Vec<Item>,
    presets: Vec<Preset>,
}
pub struct Library {
    root: PathBuf,
    config: Config,
    resources: ArchiveSet,
    model: Model,
    animation: Animation,
    prepared: HashMap<String, Prepared>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub skin_color: String,
    pub size: f32,
    pub softness: f32,
    pub physics: bool,
    pub hair_color: Option<String>,
    pub items: Vec<String>,
    pub customization: BTreeMap<String, f32>,
    pub supported: bool,
    pub secondary: bool,
    pub gravity: bool,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            skin_color: crate::skin_color::DEFAULT_SKIN.into(),
            size: 0.5,
            softness: 0.5,
            physics: true,
            hair_color: None,
            items: Vec::new(),
            customization: BTreeMap::new(),
            supported: true,
            secondary: true,
            gravity: true,
        }
    }
}
#[derive(Clone)]
struct Prepared {
    files: Vec<PathBuf>,
    menu: Option<Vec<Vec<String>>>,
}
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}
fn file_stamp(path: &str) -> Result<String> {
    let m = fs::metadata(path)?;
    Ok(format!(
        "{}:{}",
        m.len(),
        m.modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid("asset timestamp"))?
            .as_nanos()
    ))
}
fn source_signature(source: &Source) -> Result<String> {
    let mut s = serde_json::to_string(source).map_err(|e| invalid(e.to_string()))?;
    if let Source::Legacy { path, .. } = source {
        s.push_str(&file_stamp(path)?);
    }
    Ok(s)
}
fn category(name: &str) -> Option<&str> {
    match name {
        "wear" | "skirt" | "onepiece" | "mizugi" | "bra" | "panz" | "stkg" | "shoes" | "glove"
        | "acchat" | "acchead" | "acckami" | "acckamisub" | "acckubi" | "acckubiwa" | "accmimi"
        | "megane" | "accashi" | "accude" | "accheso" | "accsenaka" | "accshippo" | "handitem"
        | "hairf" | "hairr" | "hairs" | "hairt" | "hairaho" | "accnail" => Some(name),
        _ => None,
    }
}
fn absolute(path: &Path) -> Result<String> {
    Ok(path.canonicalize()?.to_string_lossy().into_owned())
}
pub fn init(
    canonical: &Path,
    model: &Path,
    animation: &Path,
    old: &Path,
    archives: &[String],
    root: &Path,
    originals: Option<&Path>,
) -> Result<Value> {
    fs::create_dir_all(root.join("parts"))?;
    fs::create_dir_all(root.join("cache"))?;
    let mut items = BTreeMap::new();
    let mut presets = Vec::new();
    let mut ignored = 0;
    let mut failures = Vec::new();
    let mut special = HashMap::new();
    for archive in archives {
        let mut a = Archive::open(Path::new(archive))?;
        let inventory = catalog::build(&mut a)?;
        for v in inventory["entries"].as_array().unwrap() {
            let cat = v["category"].as_str().unwrap_or("");
            let path = v["source"].as_str().unwrap_or("");
            if category(cat).is_none()
                || path.starts_with("man/")
                || v["models"].as_array().is_none_or(Vec::is_empty)
            {
                ignored += 1;
                continue;
            }
            let id = v["id"].as_str().unwrap().to_owned();
            items.insert(
                id.clone(),
                Item {
                    id: id.clone(),
                    name: v["name"].as_str().unwrap_or(cat).to_owned(),
                    category: cat.into(),
                    origin: "COM3D2".into(),
                    source: Source::Native { menu: path.into() },
                },
            );
            if path.ends_with("/onepiece003/menu/onep003_i_.menu") {
                special.insert("coat", id.clone());
            }
            if path.ends_with("/dress218/menu/dress218_shoe_i_.menu") {
                special.insert("boots", id);
            }
        }
        failures.extend(inventory["failures"].as_array().unwrap().iter().cloned());
    }
    if let Ok(bytes) = fs::read(old.join("report.json")) {
        let report: Value = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
        for look in report
            .as_array()
            .ok_or_else(|| invalid("legacy wardrobe report"))?
        {
            let id = look["id"].as_str().unwrap_or("");
            if ["nte-06", "nte-13", "nte-14", "nte-15", "nte-18"].contains(&id)
                || !look["error"].is_null()
            {
                continue;
            }
            let mut selected = Vec::new();
            if let Some(pieces) = look["pieces"].as_array() {
                for part in pieces {
                    let Some(file) = part["glb"].as_str() else {
                        continue;
                    };
                    let file = old.join(crate::arc::safe_relative(file)?);
                    if !file.is_file() {
                        failures.push(json!({"source":file,"error":"legacy part is missing"}));
                        continue;
                    }
                    let slot = part["slot"].as_str().unwrap_or("other");
                    let cat = match slot {
                        "tops" => "wear",
                        "bottoms" => "skirt",
                        "socks" => "stkg",
                        "gloves" => "glove",
                        "accessories" => "accessories",
                        s => s,
                    };
                    let item_id = format!("{id}-{slot}");
                    items.insert(
                        item_id.clone(),
                        Item {
                            id: item_id.clone(),
                            name: format!("{} · {slot}", look["name"].as_str().unwrap_or(id)),
                            category: cat.into(),
                            origin: "NTE".into(),
                            source: Source::Legacy {
                                path: absolute(&file)?,
                                hair: false,
                                clothes: false,
                                fit: if id == "nte-10" {
                                    Some(legacy::FitProfile {
                                        chest_vertical_scale: if slot == "tops" {
                                            1.65
                                        } else {
                                            1.
                                        },
                                        chest_offset_y: if slot == "tops" { -0.025 } else { 0. },
                                        waist_offset_y: 0.10,
                                    })
                                } else {
                                    None
                                },
                            },
                        },
                    );
                    selected.push(item_id);
                }
            }
            if !selected.is_empty() {
                presets.push(Preset {
                    id: id.into(),
                    name: look["name"].as_str().unwrap_or(id).into(),
                    items: selected,
                });
            }
        }
    }
    for hair in ["ponytail", "braid", "bun", "twintails", "bob"] {
        let path = old.join(format!("hair-{hair}.glb"));
        if path.is_file() {
            let id = format!("legacy-hair-{hair}");
            items.insert(
                id.clone(),
                Item {
                    id,
                    name: hair.into(),
                    category: "hairfull".into(),
                    origin: "Existing hair".into(),
                    source: Source::Legacy {
                        path: absolute(&path)?,
                        hair: true,
                        clothes: false,
                        fit: None,
                    },
                },
            );
        }
    }
    if let Some(dir) = originals {
        if let Ok(bytes) = fs::read(dir.join("wardrobe.json")) {
            let w: Value = serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
            let outfits = w
                .as_array()
                .or_else(|| w["outfits"].as_array())
                .ok_or_else(|| invalid("original wardrobe"))?;
            for outfit in outfits {
                let Some(file) = outfit["glb"].as_str() else {
                    continue;
                };
                let path = dir.join(crate::arc::safe_relative(file)?);
                if !path.is_file() {
                    continue;
                }
                let slug = outfit["id"].as_str().unwrap_or("outfit");
                let id = format!("original-{slug}");
                let name = outfit["name"]
                    .as_str()
                    .or_else(|| outfit["name"]["en"].as_str())
                    .unwrap_or(slug)
                    .to_owned();
                items.insert(
                    id.clone(),
                    Item {
                        id: id.clone(),
                        name: name.clone(),
                        category: "original".into(),
                        origin: "Original Secretary".into(),
                        source: Source::Legacy {
                            path: absolute(&path)?,
                            hair: false,
                            clothes: true,
                            fit: None,
                        },
                    },
                );
                presets.push(Preset {
                    id: id.clone(),
                    name,
                    items: vec![id],
                });
            }
        }
    }
    if let (Some(coat), Some(boots)) = (special.get("coat"), special.get("boots")) {
        presets.insert(
            0,
            Preset {
                id: "native-coat".into(),
                name: "Long coat & Noir boots".into(),
                items: vec![coat.clone(), boots.clone()],
            },
        );
    }
    let cfg = Config {
        version: VERSION,
        canonical: absolute(canonical)?,
        model: absolute(model)?,
        animation: absolute(animation)?,
        archives: archives.to_vec(),
        loose_surface_items: Vec::new(),
        loose_component_sizes: Default::default(),
        deferred_origins: Vec::new(),
        deferred_items: Vec::new(),
        items: items.into_values().collect(),
        presets,
    };
    fs::write(
        root.join("library-config.json"),
        serde_json::to_vec_pretty(&cfg).map_err(|e| invalid(e.to_string()))?,
    )?;
    let body = Model::read(&fs::read(model)?)?;
    fs::write(
        root.join("bindings.json"),
        serde_json::to_vec(&bindings::export(&body, canonical)?)
            .map_err(|e| invalid(e.to_string()))?,
    )?;
    fs::write(root.join("import-report.json"),serde_json::to_vec_pretty(&json!({"entries":cfg.items.len(),"excludedNonWardrobeRecords":ignored,"failures":failures})).unwrap())?;
    Ok(
        json!({"items":cfg.items.len(),"presets":cfg.presets.len(),"ignoredNonWardrobeRecords":ignored,"failures":failures.len(),"root":root}),
    )
}
fn clothes(input: &Path, output: &Path) -> Result<()> {
    let (mut doc, data) = glb_io::read(input)?;
    let materials = doc["materials"]
        .as_array()
        .ok_or_else(|| invalid("original materials"))?
        .clone();
    let mut mapping = HashMap::new();
    let mut meshes = Vec::new();
    for (i, mesh) in doc["meshes"]
        .as_array()
        .ok_or_else(|| invalid("original meshes"))?
        .iter()
        .enumerate()
    {
        let mut m = mesh.clone();
        m["primitives"].as_array_mut().unwrap().retain(|p| {
            p["material"]
                .as_u64()
                .and_then(|i| materials.get(i as usize))
                .and_then(|m| m["name"].as_str())
                .is_some_and(|n| {
                    let n = n.to_lowercase();
                    n.starts_with("clothes")
                        || n.starts_with("outfit")
                        || n.starts_with("wardrobe")
                        || n == "dress"
                })
        });
        if !m["primitives"].as_array().unwrap().is_empty() {
            mapping.insert(i, meshes.len());
            meshes.push(m);
        }
    }
    if meshes.is_empty() {
        return Err(invalid(
            "no independently separated original clothing surfaces; source kept intact",
        ));
    }
    for node in doc["nodes"].as_array_mut().unwrap() {
        if let Some(m) = node["mesh"].as_u64() {
            if let Some(&new) = mapping.get(&(m as usize)) {
                node["mesh"] = json!(new);
            } else {
                node.as_object_mut().unwrap().remove("mesh");
                node.as_object_mut().unwrap().remove("skin");
            }
        }
    }
    doc["meshes"] = json!(meshes);
    glb_io::write(output, &doc, &data)
}
impl Library {
    pub fn warm(&mut self, all: bool) -> Result<Value> {
        let mut report = Vec::new();
        {
            let focused = self
                .config
                .presets
                .iter()
                .flat_map(|p| p.items.iter())
                .collect::<HashSet<_>>();
            let mut items = self
                .config
                .items
                .iter()
                .filter(|i| self.active(i) && (all || focused.contains(&i.id)))
                .cloned()
                .collect::<Vec<_>>();

            items.sort_by_key(|i| match &i.source {
                Source::Native { menu } => menu.clone(),
                Source::Legacy { path, .. } => path.clone(),
            });
            for item in items {
                let result = self.prepare(&item.id);
                report.push(match result{Ok(p)=>{
                    let native=p.files.first().and_then(|p|p.parent()).and_then(|p|p.parent()).map(|p|p.join("manifest.json")).filter(|p|p.is_file());
                    let pack=native.as_ref().map(|p|fs::read(p).map_err(Into::into).and_then(|b|serde_json::from_slice::<Value>(&b).map_err(|e|invalid(e.to_string())))).transpose()?;
                    json!({"id":item.id,"name":item.name,"origin":item.origin,"ready":true,"parts":p.files.len(),"nativeManifest":native,"nativePack":pack})
                },Err(e)=>json!({"id":item.id,"name":item.name,"origin":item.origin,"ready":false,"error":e.to_string()})});
                if report.len() % 100 == 0 {
                    eprintln!(
                        "Prepared {}/{} wardrobe entries",
                        report.len(),
                        self.config.items.len()
                    );
                }
            }
            for entry in &mut report {
                entry["pipelineRevision"] = json!(PIPELINE_REVISION);
            }
            fs::write(
                self.root.join("asset-import-report.json"),
                serde_json::to_vec_pretty(&report).unwrap(),
            )?;
        }
        let mut looks = Vec::new();
        for preset in self
            .config
            .presets
            .clone()
            .into_iter()
            .filter(|p| {
                p.items.iter().all(|id| {
                    self.config
                        .items
                        .iter()
                        .any(|i| &i.id == id && self.active(i))
                })
            })
            .collect::<Vec<_>>()
        {
            eprintln!("Composing {}", preset.id);
            looks.push(match self.compose(&preset.items) {
                Ok(result) => {
                    json!({"id":preset.id,"name":preset.name,"ready":true,"result":result})
                }
                Err(e) => {
                    json!({"id":preset.id,"name":preset.name,"ready":false,"error":e.to_string()})
                }
            });
        }
        for entry in &mut looks {
            entry["pipelineRevision"] = json!(PIPELINE_REVISION);
        }
        fs::write(
            self.root.join("preset-import-report.json"),
            serde_json::to_vec_pretty(&looks).unwrap(),
        )?;
        Ok(
            json!({"assets":report.len(),"readyAssets":report.iter().filter(|r|r["ready"]==true).count(),"failedAssets":report.iter().filter(|r|r["ready"]==false).count(),"presets":looks.len(),"readyPresets":looks.iter().filter(|r|r["ready"]==true).count(),"failedPresets":looks.iter().filter(|r|r["ready"]==false).collect::<Vec<_>>()}),
        )
    }
    pub fn open(root: &Path) -> Result<Self> {
        let config: Config = serde_json::from_slice(&fs::read(root.join("library-config.json"))?)
            .map_err(|e| invalid(e.to_string()))?;
        if config.version != VERSION {
            return Err(invalid("wardrobe schema version"));
        }
        let resources = ArchiveSet::open(&config.archives)?;
        let model = Model::read(&fs::read(&config.model)?)?;
        let animation = Animation::read(&fs::read(&config.animation)?)?;
        Ok(Self {
            root: root.into(),
            config,
            resources,
            model,
            animation,
            prepared: HashMap::new(),
        })
    }
    fn active(&self, item: &Item) -> bool {
        !self.config.deferred_origins.contains(&item.origin)
            && !self.config.deferred_items.contains(&item.id)
    }
    /// Offered in the catalog and accepted from a request: active and safe for work.
    fn selectable(&self, item: &Item) -> bool {
        self.active(item) && safe_item(&item.name, &item.category)
    }
    /// The plain inner item of `category` ("bra" or "panz") the coverage guard adds.
    fn inner(&self, category: &str, name: &str) -> Option<String> {
        self.config
            .items
            .iter()
            .find(|i| i.category == category && i.name == name && self.active(i))
            .map(|i| i.id.clone())
    }
    /// Plain inner wear for whatever `ids` leave uncovered, so the bare body is never shown.
    fn cover(&self, ids: &mut Vec<String>) {
        let cats = ids
            .iter()
            .filter_map(|id| self.config.items.iter().find(|i| &i.id == id))
            .map(|i| i.category.as_str());
        let (top, bottom) = coverage(cats);
        for (covered, category, name) in [
            (top, "bra", "シンプルインナーブラ"),
            (bottom, "panz", "シンプルインナースパッツ"),
        ] {
            if !covered {
                ids.extend(self.inner(category, name));
            }
        }
        ids.sort();
    }
    pub fn catalog(&self) -> Value {
        let reports = |name: &str| -> HashMap<String, Value> {
            fs::read(self.root.join(name))
                .ok()
                .and_then(|b| serde_json::from_slice::<Vec<Value>>(&b).ok())
                .unwrap_or_default()
                .into_iter()
                .filter(|v| v["pipelineRevision"] == PIPELINE_REVISION)
                .filter_map(|v| Some((v["id"].as_str()?.to_owned(), v)))
                .collect()
        };
        let assets = reports("asset-import-report.json");
        let looks = reports("preset-import-report.json");
        json!({"version":VERSION,"items":self.config.items.iter().filter(|i|self.selectable(i)).map(|i|json!({"id":i.id,"name":i.name,"category":i.category,"origin":i.origin,"fitReviewRequired":true,"ready":assets.get(&i.id).map(|r|r["ready"].clone()),"error":assets.get(&i.id).map(|r|r["error"].clone())})).collect::<Vec<_>>(),"presets":self.config.presets.iter().filter(|p|p.items.iter().all(|id|self.config.items.iter().find(|i|&i.id==id).is_some_and(|i|self.selectable(i)))).map(|p|json!({"id":p.id,"name":p.name,"items":p.items,"ready":looks.get(&p.id).map(|r|r["ready"].clone()),"error":looks.get(&p.id).map(|r|r["error"].clone())})).collect::<Vec<_>>(),"skinColor":crate::skin_color::DEFAULT_SKIN,"allAssetLogic":"Rust","excluded":"head/body replacements, male donor characters and non-wardrobe game records"})
    }
    pub fn settings(&self) -> Result<Settings> {
        let path = self.root.join("avatar-settings.json");
        if !path.is_file() {
            let s = Settings {
                items: self
                    .config
                    .presets
                    .first()
                    .map(|p| p.items.clone())
                    .unwrap_or_default(),
                ..Settings::default()
            };
            return Ok(s);
        }
        let s = serde_json::from_slice(&fs::read(path)?).map_err(|e| invalid(e.to_string()))?;
        self.validate_settings(&s)?;
        Ok(s)
    }
    fn validate_settings(&self, s: &Settings) -> Result<()> {
        crate::skin_color::color(&s.skin_color)?;
        crate::customization::values(&s.customization)?;
        if let Some(c) = &s.hair_color {
            crate::skin_color::color(c)?;
        }
        if !s.size.is_finite()
            || !(0.0..=1.0).contains(&s.size)
            || !s.softness.is_finite()
            || !(0.0..=1.0).contains(&s.softness)
            || s.items.len() > 40
        {
            return Err(invalid("avatar setting range"));
        }
        for id in &s.items {
            if !self
                .config
                .items
                .iter()
                .any(|i| &i.id == id && self.selectable(i))
            {
                return Err(invalid("unknown saved wardrobe item"));
            }
        }
        Ok(())
    }
    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        self.validate_settings(s)?;
        let path = self.root.join("avatar-settings.json.tmp");
        fs::write(
            &path,
            serde_json::to_vec_pretty(s).map_err(|e| invalid(e.to_string()))?,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        fs::rename(path, self.root.join("avatar-settings.json"))?;
        Ok(())
    }
    fn prepare(&mut self, id: &str) -> Result<Prepared> {
        if let Some(p) = self.prepared.get(id) {
            return Ok(p.clone());
        }
        let item = self
            .config
            .items
            .iter()
            .find(|i| i.id == id)
            .cloned()
            .ok_or_else(|| invalid(format!("unknown wardrobe item: {id}")))?;
        if !self.active(&item) {
            return Err(invalid(format!(
                "wardrobe item deferred for gravity re-authoring: {id}"
            )));
        }
        let stem = format!("part-v2-{:016x}", hash(id));
        let mut files = Vec::new();
        let mut menu = None;
        match &item.source {
            Source::Legacy {
                path,
                hair,
                clothes: extract,
                fit,
            } => {
                let fit_key =
                    hash(&serde_json::to_string(fit).map_err(|e| invalid(e.to_string()))?);
                let source_key = hash(&format!(
                    "{}:{}",
                    file_stamp(path)?,
                    file_stamp(&self.config.canonical)?
                ));
                let out = self
                    .root
                    .join("parts")
                    .join(format!("{stem}-fit15-{fit_key:016x}-{source_key:016x}.glb"));
                if !out.is_file() {
                    let source = if *extract {
                        let filtered = self
                            .root
                            .join("parts")
                            .join(format!("{stem}-clothing-source.glb"));
                        clothes(Path::new(path), &filtered)?;
                        filtered
                    } else {
                        PathBuf::from(path)
                    };
                    let reference = if *hair {
                        None
                    } else {
                        let signature = hash(&format!(
                            "{}:{}",
                            file_stamp(&self.config.canonical)?,
                            file_stamp(&self.config.animation)?
                        ));
                        let pose = self
                            .root
                            .join("parts")
                            .join(format!("reference-standing-{signature:016x}.glb"));
                        if !pose.is_file() {
                            pose_asset::standing(
                                &self.model,
                                &self.animation,
                                Path::new(&self.config.canonical),
                                &pose,
                                0.,
                            )?;
                        }
                        Some(pose)
                    };
                    legacy::retarget_reference(
                        &source,
                        Path::new(&self.config.canonical),
                        reference.as_deref(),
                        &out,
                        *hair,
                        fit.as_ref(),
                        item.id == "original-uniform",
                    )?;
                }
                let native_dir = self
                    .root
                    .join("native")
                    .join(format!("{stem}-fit15-{fit_key:016x}-{source_key:016x}-v7"));
                let manifest = native_dir.join("manifest.json");
                let pack = if manifest.is_file() {
                    serde_json::from_slice::<Value>(&fs::read(&manifest)?)
                        .map_err(|e| invalid(e.to_string()))?
                } else {
                    crate::native_pack::export(
                        &out,
                        &self.model,
                        &native_dir,
                        if item.id == "original-uniform" {
                            "original-uniform"
                        } else if item.category == "original"
                            && !["original-gym", "original-crimson", "original-uniform"]
                                .contains(&item.id.as_str())
                        {
                            "original-skirt"
                        } else {
                            &item.category
                        },
                    )?
                };
                if pack["version"] != 7 {
                    return Err(invalid("native wardrobe pack version"));
                }
                let rules = crate::native_pack::refresh_body_masks(
                    &native_dir,
                    pack["requestedCategory"].as_str().unwrap_or(&item.category),
                )?;
                if !rules.is_empty() {
                    menu = Some(rules);
                }
                for path in pack["render"]
                    .as_array()
                    .ok_or_else(|| invalid("native wardrobe render list"))?
                {
                    let file = native_dir.join(crate::arc::safe_relative(
                        path.as_str()
                            .ok_or_else(|| invalid("native wardrobe render path"))?,
                    )?);
                    crate::native_cloth::annotate_mantle_file(&file)?;
                    files.push(file);
                }
            }
            Source::Native { menu: path } => {
                let m = Menu::read(&self.resources.read_resource(path)?)?;
                let mut index = 0;
                for command in &m.commands {
                    if command.first().is_none_or(|s| s != "additem") || command.len() < 3 {
                        continue;
                    }
                    let source = self.resources.resolve(&command[1], path)?;
                    if !source.to_lowercase().ends_with(".model") {
                        return Err(invalid("native item references a non-model resource"));
                    }
                    let out = self.root.join("parts").join(format!("{stem}-{index}.glb"));
                    index += 1;
                    if !out.is_file() {
                        let mut model = Model::read(&self.resources.read_resource(&source)?)?;
                        for c in &m.commands {
                            if c.len() >= 4
                                && c[0] == "マテリアル変更"
                                && c[1].eq_ignore_ascii_case(&command[2])
                            {
                                let slot = c[2]
                                    .parse::<usize>()
                                    .map_err(|_| invalid("native material slot"))?;
                                let path = self.resources.resolve(&c[3], path)?;
                                let material = mate::read(&self.resources.read_resource(&path)?)?;
                                *model
                                    .materials
                                    .get_mut(slot)
                                    .ok_or_else(|| invalid("native material slot bounds"))? =
                                    material;
                            }
                        }
                        let staging = out.with_extension("staging.glb");
                        gltf::export_model(&mut self.resources, &source, &model, &staging)?;
                        if model
                            .materials
                            .iter()
                            .any(|m| crate::skin_assets::is_skin(&m.name))
                        {
                            crate::skin_assets::normalize(&staging, &staging, None)?;
                        }
                        fs::rename(staging, &out)?;
                    }
                    crate::native_cloth::annotate_file(&mut self.resources, &source, &out)?;
                    files.push(out);
                }
                if files.is_empty() {
                    return Err(invalid("native menu has no renderable model"));
                }
                menu = Some(m.commands);
            }
        }
        let prepared = Prepared { files, menu };
        self.prepared.insert(id.into(), prepared.clone());
        Ok(prepared)
    }
    pub fn compose(&mut self, requested: &[String]) -> Result<Value> {
        if requested.len() > 40 {
            return Err(invalid("at most 40 wardrobe layers"));
        }
        let ids = requested
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect::<HashSet<_>>();
        if ids.len() != requested.len() {
            return Err(invalid("duplicate wardrobe layer"));
        }
        let mut ids = ids.into_iter().collect::<Vec<_>>();
        ids.sort();
        for id in &ids {
            if !self
                .config
                .items
                .iter()
                .any(|i| &i.id == id && self.selectable(i))
            {
                return Err(invalid(format!("unknown wardrobe item: {id}")));
            }
        }
        self.cover(&mut ids);
        let modified = fs::metadata(&self.config.canonical)?
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| invalid("canonical timestamp"))?
            .as_nanos();
        let profiles = ids
            .iter()
            .map(|id| self.config.items.iter().find(|i| &i.id == id).unwrap())
            .map(|i| source_signature(&i.source))
            .collect::<Result<Vec<_>>>()?
            .join(";");
        let key = hash(&format!(
            "v{VERSION}-native30:{modified}:{}:{profiles}:{:?}:{:?}",
            ids.join(","),
            self.config.loose_surface_items,
            self.config.loose_component_sizes
        ));
        let filename = format!("view-{key:016x}.glb");
        let output = self.root.join("cache").join(&filename);
        if output.is_file() {
            return Ok(
                json!({"url":format!("./cache/{filename}"),"items":ids,"cached":true,"body":"native COM3D2 with real Lucina head"}),
            );
        }
        let work = self.root.join("cache").join(format!("work-{key:016x}.glb"));
        fs::copy(&self.config.canonical, &work)?;
        let selected = ids
            .iter()
            .map(|id| {
                self.config
                    .items
                    .iter()
                    .find(|i| &i.id == id)
                    .unwrap()
                    .clone()
            })
            .collect::<Vec<_>>();
        let prepared = ids
            .iter()
            .map(|id| self.prepare(id))
            .collect::<Result<Vec<_>>>()?;
        if selected.iter().any(|i| {
            ["hairfull", "hairf", "hairr", "hairs", "hairt"].contains(&i.category.as_str())
        }) {
            let (mut doc, data) = glb_io::read(&work)?;
            for m in doc["materials"].as_array_mut().unwrap() {
                if m["name"]
                    .as_str()
                    .is_some_and(|n| n == "Hair" || n == "HairHighlight")
                {
                    m["pbrMetallicRoughness"]["baseColorFactor"] = json!([0, 0, 0, 0]);
                    m["alphaMode"] = json!("BLEND");
                    m["extras"]["baseHairHidden"] = json!(true);
                }
            }
            glb_io::write(&work, &doc, &data)?;
        }
        let mut hidden = HashSet::new();
        for p in &prepared {
            if let Some(commands) = &p.menu {
                for c in commands {
                    if c.len() >= 2 && c[0] == "maskitem" {
                        hidden.insert(c[1].clone());
                    }
                }
            }
        }
        for p in &prepared {
            if let Some(commands) = &p.menu {
                if commands.iter().any(|c| {
                    c.first()
                        .is_some_and(|s| s == "node消去" || s == "node表示")
                }) {
                    let menu = Menu {
                        version: 2001,
                        source: "selected native wardrobe layers".into(),
                        name: String::new(),
                        category: String::new(),
                        description: String::new(),
                        commands: commands.clone(),
                    };
                    masking::body(&work, &menu, &work)?;
                }
            }
        }
        let mut attached = 0;
        for (item, p) in selected.iter().zip(&prepared) {
            if hidden.contains(&item.category) {
                continue;
            }
            for file in &p.files {
                let accessory = self.config.loose_surface_items.contains(&item.id);
                let temporary = work.with_extension("hanging.glb");
                let mut wearable = file.clone();
                if accessory {
                    let (mut doc, data) = crate::glb_io::read(file)?;
                    let sizes = self
                        .config
                        .loose_component_sizes
                        .get(&item.id)
                        .ok_or_else(|| invalid("loose item needs reviewed component sizes"))?;
                    for node in doc["nodes"].as_array_mut().unwrap() {
                        if node["mesh"].is_number() {
                            node["extras"]["secretaryRibbonSizes"] = json!(sizes);
                        }
                    }
                    crate::glb_io::write(&temporary, &doc, &data)?;
                    wearable = temporary.clone();
                }
                compose::attach(&work, &wearable, &work)?;
                let _ = fs::remove_file(temporary);

                attached += 1;
                if fs::metadata(&work)?.len() > 256 * 1024 * 1024 {
                    return Err(invalid("composed wardrobe exceeds 256 MiB"));
                }
            }
        }
        crate::secondary_bindings::annotate(&work, &work)?;
        crate::customization::annotate_garment_bindings(&work)?;
        pose_asset::standing(&self.model, &self.animation, &work, &work, 0.)?;
        crate::secondary_rig::posed_ribbons(&work)?;
        crate::native_cloth::add_back_cage(&work)?;
        crate::secondary_bindings::annotate(&work, &output)?;
        let _ = fs::remove_file(&work);
        Ok(
            json!({"url":format!("./cache/{filename}"),"items":ids,"attachedParts":attached,"maskedSlots":hidden,"cached":false,"body":"native COM3D2 with real Lucina head","hairClothCollisionParity":false}),
        )
    }
}
#[cfg(test)]
mod safety_tests {
    use super::*;
    #[test]
    fn adult_items_swimwear_and_bare_underwear_are_never_offered() {
        for (name, category) in [
            ("スカートめくれ", "skirt"),
            ("バスタオルずらし_z1", "mizugi"),
            ("セクシーサキュバスボンデージ・青", "mizugi"),
            ("茶色いバイブ", "handitem"),
            ("Ｃｈｕ-Ｂ Ｌｉｐ Ｂｌａｃｋ", "handitem"),
            ("SM用アイマスク・紫　アイマスク", "megane"),
            ("犬用の首輪・紫　首輪", "acckubiwa"),
            ("ラブ・ナースキャップ", "headset"),
            ("ネーブルバニースーツ・黄", "mizugi"),
            ("シンプルインナーブラ", "bra"),
            ("シンプルインナースパッツ", "panz"),
            ("コート＆ワンピース・白黒", "onepiece"),
        ] {
            let expected = name == "コート＆ワンピース・白黒";
            assert_eq!(safe_item(name, category), expected, "{name}");
        }
        for (name, category) in [
            ("クレールメイドウェア", "wear"),
            ("ティーカップ", "handitem"),
            ("ネコミミ・白　帽子", "acchat"),
            ("ロングポニーテール　ポニーテール", "hairt"),
        ] {
            assert!(safe_item(name, category), "{name}");
        }
    }
    #[test]
    fn coverage_needs_a_top_and_a_bottom_or_a_whole_outfit() {
        assert_eq!(coverage([]), (false, false));
        assert_eq!(coverage(["shoes", "acchat"]), (false, false));
        assert_eq!(coverage(["wear"]), (true, false));
        assert_eq!(coverage(["skirt", "stkg"]), (false, true));
        assert_eq!(coverage(["wear", "skirt"]), (true, true));
        assert_eq!(coverage(["onepiece"]), (true, true));
        assert_eq!(coverage(["original"]), (true, true));
    }
}
