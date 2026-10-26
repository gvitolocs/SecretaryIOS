//! `icons`: tooling for the shopping item icons in `assets/icons/` (style guide: docs/design/item-icons.md).
//!
//! - `icons validate [--palette P] [--max-bytes N] <files|dirs>…` checks the style-guide contract.
//! - `icons render <in.svg> <out.png> [--size N] [--bg #hex] [--outline]` rasterises one icon with resvg.
//! - `icons sheet --out <png> [options] <files|dirs>…` lays icons out on red and teal tiles (contact sheet).
//! - `icons outline <in.svg> <out.svg>` writes the variant-A (1.5 px ink outline) form of an icon.
//! - `icons contrast [--outline] <files|dirs>…` measures how well each silhouette separates from the tile colours.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use resvg::{tiny_skia, usvg};

const INK: &str = "#3d2b2b";
const OUTLINE_WIDTH: f32 = 1.5;
/// Content must stay inside [PAD, 64 - PAD].
const PAD: f32 = 2.0;
const DEFAULT_MAX_BYTES: usize = 2560;

const USAGE: &str = "usage:
  icons validate [--palette P] [--max-bytes N] <files|dirs>...
  icons render <in.svg> <out.png> [--size N] [--bg #hex] [--outline|--silhouette]
  icons sheet --out <png> [--scale S] [--cols N] [--tiles spec|web|all] [--theme light|dark]
              [--outline|--silhouette] [--shadow] [--catalog catalog.v1.json] [--title T] <files|dirs>...
  icons outline <in.svg> <out.svg>
  icons contrast [--outline|--silhouette] [--size 58] [--tiles #hex,#hex,...] [--max-weak 25] <files|dirs>...";

/// Elements an icon may contain. Everything else (script, image, text, style, filter, a, use, foreignObject,
/// animation, …) is rejected.
const ALLOWED_ELEMENTS: &[&str] = &[
    "svg", "g", "path", "circle", "ellipse", "rect", "polygon", "polyline", "line", "defs", "linearGradient",
    "radialGradient", "stop", "clipPath", "mask",
];
const COLOR_ATTRS: &[&str] = &["fill", "stroke", "stop-color", "color", "flood-color", "lighting-color"];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]);
    let result = match args.first().map(String::as_str) {
        Some("validate") => cmd_validate(rest),
        Some("render") => cmd_render(rest),
        Some("sheet") => cmd_sheet(rest),
        Some("outline") => cmd_outline(rest),
        Some("contrast") => cmd_contrast(rest),
        _ => Err(USAGE.to_string()),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

// ---------- argument helpers ----------

struct Args {
    flags: BTreeMap<String, String>,
    switches: BTreeSet<String>,
    positional: Vec<String>,
}

fn parse_args(args: &[String], valued: &[&str]) -> Result<Args, String> {
    let mut out = Args { flags: BTreeMap::new(), switches: BTreeSet::new(), positional: vec![] };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            if valued.contains(&name) {
                let v = args.get(i + 1).ok_or_else(|| format!("--{name} needs a value"))?;
                out.flags.insert(name.to_string(), v.clone());
                i += 1;
            } else {
                out.switches.insert(name.to_string());
            }
        } else {
            out.positional.push(a.clone());
        }
        i += 1;
    }
    Ok(out)
}

fn collect_svgs(paths: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut out = vec![];
    for p in paths {
        let path = PathBuf::from(p);
        if path.is_dir() {
            let mut files: Vec<PathBuf> = fs::read_dir(&path)
                .map_err(|e| format!("{p}: {e}"))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.extension().is_some_and(|x| x == "svg"))
                .collect();
            files.sort();
            out.extend(files);
        } else {
            out.push(path);
        }
    }
    if out.is_empty() {
        return Err("no SVG files given".into());
    }
    Ok(out)
}

fn default_palette_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("palette.json")
}

/// hex (lowercase #rrggbb) -> palette name
fn load_palette(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let obj = v.as_object().ok_or("palette must be a JSON object")?;
    let mut out = BTreeMap::new();
    for (name, hex) in obj {
        if name.starts_with('_') {
            continue;
        }
        let hex = hex.as_str().ok_or_else(|| format!("palette {name}: not a string"))?;
        let norm = normalise_hex(hex).ok_or_else(|| format!("palette {name}: bad colour {hex}"))?;
        out.insert(norm, name.clone());
    }
    Ok(out)
}

fn normalise_hex(v: &str) -> Option<String> {
    let h = v.trim().strip_prefix('#')?;
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let h = h.to_ascii_lowercase();
    match h.len() {
        3 => Some(format!("#{}", h.chars().flat_map(|c| [c, c]).collect::<String>())),
        6 => Some(format!("#{h}")),
        _ => None,
    }
}

fn font_options() -> usvg::Options<'static> {
    let mut opt = usvg::Options::default();
    opt.fontdb_mut().load_system_fonts();
    opt.font_family = "DejaVu Sans".into();
    opt
}

fn parse_tree(text: &str, opt: &usvg::Options) -> Result<usvg::Tree, String> {
    usvg::Tree::from_str(text, opt).map_err(|e| e.to_string())
}

fn parse_color(hex: &str) -> Result<tiny_skia::Color, String> {
    let n = normalise_hex(hex).ok_or_else(|| format!("bad colour {hex}"))?;
    let b = |i: usize| u8::from_str_radix(&n[i..i + 2], 16).unwrap();
    Ok(tiny_skia::Color::from_rgba8(b(1), b(3), b(5), 255))
}

fn render_tree(tree: &usvg::Tree, scale: f32, bg: Option<tiny_skia::Color>) -> Result<tiny_skia::Pixmap, String> {
    let size = tree.size();
    let w = (size.width() * scale).ceil() as u32;
    let h = (size.height() * scale).ceil() as u32;
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or("empty pixmap")?;
    if let Some(bg) = bg {
        pixmap.fill(bg);
    }
    resvg::render(tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(pixmap)
}

// ---------- validate ----------

#[derive(Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
    info: String,
}

fn opacity_of(n: roxmltree::Node) -> f32 {
    let mut o = 1.0f32;
    for a in n.ancestors().filter(|a| a.is_element()) {
        for attr in ["opacity", "fill-opacity"] {
            if let Some(v) = a.attribute(attr).and_then(|v| v.trim().parse::<f32>().ok()) {
                o *= v;
            }
        }
    }
    o
}

fn validate_one(path: &Path, palette: &BTreeMap<String, String>, max_bytes: usize, opt: &usvg::Options) -> Report {
    let mut r = Report::default();
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            r.errors.push(format!("read: {e}"));
            return r;
        }
    };
    if text.len() > max_bytes {
        r.errors.push(format!("size {} B > budget {max_bytes} B", text.len()));
    }
    let doc = match roxmltree::Document::parse(&text) {
        Ok(d) => d,
        Err(e) => {
            r.errors.push(format!("not well-formed XML: {e}"));
            return r;
        }
    };
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some("http://www.w3.org/2000/svg") {
        r.errors.push("root element must be <svg xmlns=\"http://www.w3.org/2000/svg\">".into());
    }
    let vb: Vec<String> = root
        .attribute("viewBox")
        .unwrap_or("")
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if vb != ["0", "0", "64", "64"] {
        r.errors.push(format!("viewBox must be \"0 0 64 64\", got {:?}", root.attribute("viewBox")));
    }

    let mut gradients = 0;
    let mut opaque_fills: BTreeSet<String> = BTreeSet::new();
    for n in doc.descendants().filter(|n| n.is_element()) {
        let tag = n.tag_name().name();
        if !ALLOWED_ELEMENTS.contains(&tag) {
            r.errors.push(format!("forbidden element <{tag}>"));
        }
        if tag.ends_with("Gradient") {
            gradients += 1;
        }
        for a in n.attributes() {
            let name = a.name();
            let value = a.value();
            if name.starts_with("on") {
                r.errors.push(format!("event handler attribute {name} on <{tag}>"));
            }
            if name == "style" {
                r.errors.push(format!("style attribute on <{tag}>: use presentation attributes"));
            }
            if name == "href" && !value.starts_with('#') {
                r.errors.push(format!("external reference {name}=\"{value}\""));
            }
            if value.contains("url(") && !value.trim_start().starts_with("url(#") {
                r.errors.push(format!("external url() in {name}=\"{value}\""));
            }
            if COLOR_ATTRS.contains(&name) {
                let v = value.trim();
                if v == "none" || v.starts_with("url(#") {
                    continue;
                }
                match normalise_hex(v) {
                    Some(hex) if palette.contains_key(&hex) => {
                        if (name == "fill" || name == "stroke") && opacity_of(n) >= 0.99 {
                            opaque_fills.insert(palette[&hex].clone());
                        }
                    }
                    _ => r.errors.push(format!("colour {name}=\"{v}\" on <{tag}> is not in the palette")),
                }
            }
        }
    }
    match gradients {
        0 => {}
        1 => r.warnings.push("uses a gradient: keep it tiny (style guide)".into()),
        n => r.errors.push(format!("{n} gradients: at most one tiny gradient")),
    }
    if !(3..=7).contains(&opaque_fills.len()) {
        r.warnings.push(format!(
            "{} flat colours ({}); the guide asks for 3-7",
            opaque_fills.len(),
            opaque_fills.iter().cloned().collect::<Vec<_>>().join(",")
        ));
    }

    match parse_tree(&text, opt) {
        Ok(tree) => {
            let b = tree.root().abs_stroke_bounding_box();
            let (l, t, rr, bb) = (b.left(), b.top(), b.right(), b.bottom());
            let eps = 0.05;
            if l < -eps || t < -eps || rr > 64.0 + eps || bb > 64.0 + eps {
                r.errors.push(format!("clipped: content box {l:.1},{t:.1} - {rr:.1},{bb:.1} leaves the 64x64 viewBox"));
            } else if l < PAD - eps || t < PAD - eps || rr > 64.0 - PAD + eps || bb > 64.0 - PAD + eps {
                r.errors.push(format!("padding: content box {l:.1},{t:.1} - {rr:.1},{bb:.1} enters the {PAD} px safe margin"));
            }
            let extent = (rr - l).max(bb - t);
            if extent < 44.0 {
                r.warnings.push(format!("small: content spans only {extent:.0}/64"));
            }
            r.info = format!(
                "{} B, box {l:.1},{t:.1}-{rr:.1},{bb:.1}, colours {}",
                text.len(),
                opaque_fills.iter().cloned().collect::<Vec<_>>().join(",")
            );
        }
        Err(e) => r.errors.push(format!("resvg cannot parse: {e}")),
    }
    r
}

fn cmd_validate(args: &[String]) -> Result<ExitCode, String> {
    let a = parse_args(args, &["palette", "max-bytes"])?;
    let palette = load_palette(&a.flags.get("palette").map(PathBuf::from).unwrap_or_else(default_palette_path))?;
    let max_bytes = match a.flags.get("max-bytes") {
        Some(v) => v.parse().map_err(|_| "bad --max-bytes")?,
        None => DEFAULT_MAX_BYTES,
    };
    let opt = usvg::Options::default();
    let files = collect_svgs(&a.positional)?;
    let (mut errs, mut warns) = (0, 0);
    for f in &files {
        let r = validate_one(f, &palette, max_bytes, &opt);
        let status = if !r.errors.is_empty() { "FAIL" } else if !r.warnings.is_empty() { "warn" } else { "ok  " };
        println!("{status} {} {}", f.display(), r.info);
        for e in &r.errors {
            println!("     error: {e}");
        }
        for w in &r.warnings {
            println!("     warning: {w}");
        }
        errs += r.errors.len();
        warns += r.warnings.len();
    }
    println!("{} files, {errs} errors, {warns} warnings", files.len());
    Ok(if errs > 0 { ExitCode::from(1) } else { ExitCode::SUCCESS })
}

// ---------- outline (variant A) ----------

/// Adds a 1.5 px ink stroke to every opaque filled shape (highlights and shadows, which carry an opacity, and
/// shapes that already have a stroke are left alone).
fn outline_svg(text: &str) -> Result<String, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let mut inserts = vec![];
    for n in doc.descendants().filter(|n| n.is_element()) {
        let tag = n.tag_name().name();
        if !matches!(tag, "path" | "circle" | "ellipse" | "rect" | "polygon") {
            continue;
        }
        if n.ancestors().any(|a| matches!(a.tag_name().name(), "defs" | "clipPath" | "mask")) {
            continue;
        }
        if n.attribute("stroke").is_some() || n.attribute("fill") == Some("none") || opacity_of(n) < 0.99 {
            continue;
        }
        inserts.push(n.range().start + 1 + tag.len());
    }
    let attr = format!(r#" stroke="{INK}" stroke-width="{OUTLINE_WIDTH}" stroke-linejoin="round""#);
    let mut out = text.to_string();
    for pos in inserts.into_iter().rev() {
        out.insert_str(pos, &attr);
    }
    Ok(out)
}

/// Variant A' for evaluation only: one 1.5 px ink outline around the whole silhouette (no inner lines), drawn
/// with a dilate filter. Real icons never use filters; this only renders the comparison.
fn silhouette_svg(text: &str) -> Result<String, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    let inner = match (root.first_child(), root.last_child()) {
        (Some(f), Some(l)) => &text[f.range().start..l.range().end],
        _ => "",
    };
    Ok(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><defs><filter id="sil" x="-10%" y="-10%" width="120%" height="120%"><feMorphology in="SourceAlpha" operator="dilate" radius="{OUTLINE_WIDTH}" result="d"/><feFlood flood-color="{INK}"/><feComposite in2="d" operator="in"/><feMerge><feMergeNode/><feMergeNode in="SourceGraphic"/></feMerge></filter></defs><g filter="url(#sil)">{inner}</g></svg>"##
    ))
}

/// Applies the variant switches (`--outline`: every shape outlined; `--silhouette`: outer contour only).
fn apply_variant(text: String, a: &Args) -> Result<String, String> {
    if a.switches.contains("outline") {
        outline_svg(&text)
    } else if a.switches.contains("silhouette") {
        silhouette_svg(&text)
    } else {
        Ok(text)
    }
}

fn cmd_outline(args: &[String]) -> Result<ExitCode, String> {
    let [input, output] = args else { return Err(USAGE.into()) };
    let text = fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
    fs::write(output, outline_svg(&text)?).map_err(|e| format!("{output}: {e}"))?;
    Ok(ExitCode::SUCCESS)
}

// ---------- render ----------

fn cmd_render(args: &[String]) -> Result<ExitCode, String> {
    let a = parse_args(args, &["size", "bg"])?;
    let [input, output] = a.positional.as_slice() else { return Err(USAGE.into()) };
    let size: f32 = a.flags.get("size").map(|s| s.parse().map_err(|_| "bad --size")).transpose()?.unwrap_or(256.0);
    let bg = a.flags.get("bg").map(|c| parse_color(c)).transpose()?;
    let text = apply_variant(fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?, &a)?;
    let tree = parse_tree(&text, &usvg::Options::default())?;
    let pixmap = render_tree(&tree, size / tree.size().width(), bg)?;
    pixmap.save_png(output).map_err(|e| format!("{output}: {e}"))?;
    Ok(ExitCode::SUCCESS)
}

// ---------- contact sheet ----------

const TILE: f32 = 108.0;
const ICON: f32 = 58.0;

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The icon as a nested <svg> placed at (x, y) with size `size`, ids prefixed to stay unique in the sheet.
fn nested_icon(text: &str, prefix: &str, x: f32, y: f32, size: f32) -> Result<String, String> {
    let doc = roxmltree::Document::parse(text).map_err(|e| e.to_string())?;
    let root = doc.root_element();
    let mut attrs = String::new();
    for a in root.attributes() {
        if !matches!(a.name(), "viewBox" | "width" | "height" | "xmlns") {
            attrs.push_str(&format!(r#" {}="{}""#, a.name(), xml_escape(a.value())));
        }
    }
    let inner = match (root.first_child(), root.last_child()) {
        (Some(f), Some(l)) => &text[f.range().start..l.range().end],
        _ => "",
    };
    let inner = inner
        .replace("id=\"", &format!("id=\"{prefix}-"))
        .replace("url(#", &format!("url(#{prefix}-"))
        .replace("href=\"#", &format!("href=\"#{prefix}-"));
    Ok(format!(r#"<svg x="{x}" y="{y}" width="{size}" height="{size}" viewBox="0 0 64 64"{attrs}>{inner}</svg>"#))
}

fn catalog_names(path: Option<&String>) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    let Some(path) = path else { return Ok(out) };
    let text = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    for (kind, list) in [("section", "sections"), ("item", "items")] {
        for e in v[list].as_array().into_iter().flatten() {
            if let (Some(id), Some(name)) = (e["id"].as_str(), e["names"]["en"].as_str()) {
                out.insert(format!("{kind}:{id}"), name.to_string());
            }
        }
    }
    Ok(out)
}

fn cmd_sheet(args: &[String]) -> Result<ExitCode, String> {
    let a = parse_args(args, &["out", "scale", "cols", "tiles", "theme", "catalog", "title"])?;
    let out = a.flags.get("out").ok_or("--out is required")?;
    let scale: f32 = a.flags.get("scale").map(|s| s.parse().map_err(|_| "bad --scale")).transpose()?.unwrap_or(1.0);
    let cols: usize = a.flags.get("cols").map(|s| s.parse().map_err(|_| "bad --cols")).transpose()?.unwrap_or(5);
    let tiles: Vec<(&str, &str)> = match a.flags.get("tiles").map(String::as_str).unwrap_or("spec") {
        "spec" => vec![("#f16a6f", "red"), ("#6eb8ae", "teal")],
        "web" => vec![("#ff3b6b", "web red"), ("#14cfc1", "web teal")],
        "all" => vec![("#f16a6f", "red"), ("#6eb8ae", "teal"), ("#ff3b6b", "web red"), ("#14cfc1", "web teal")],
        t => return Err(format!("unknown --tiles {t}")),
    };
    let dark = a.flags.get("theme").map(String::as_str) == Some("dark");
    let (page, caption) = if dark { ("#111114", "#c7c7cc") } else { ("#f2f2f7", "#3a3a3c") };
    let outline = a.switches.contains("outline");
    let shadow = a.switches.contains("shadow");
    let names = catalog_names(a.flags.get("catalog"))?;
    let files = collect_svgs(&a.positional)?;

    let (margin, gap, group_gap, head, caption_h) = (16.0, 4.0, 18.0, 40.0, 18.0);
    let group_w = tiles.len() as f32 * TILE + (tiles.len() - 1) as f32 * gap;
    let cols = cols.min(files.len()).max(1);
    let rows = files.len().div_ceil(cols);
    let width = margin * 2.0 + cols as f32 * group_w + (cols - 1) as f32 * group_gap;
    let row_h = TILE + caption_h + 10.0;
    let height = head + margin + rows as f32 * row_h;

    let mut svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}"><rect width="100%" height="100%" fill="{page}"/>"#
    );
    if shadow {
        svg.push_str(r##"<defs><filter id="ds" x="-30%" y="-30%" width="160%" height="170%"><feDropShadow dx="0" dy="3" stdDeviation="3" flood-color="#000000" flood-opacity="0.25"/></filter></defs>"##);
    }
    let title = a.flags.get("title").cloned().unwrap_or_else(|| {
        format!(
            "{} icons, {}, {} theme{}{}",
            files.len(),
            if outline {
                "variant A (outline on every shape)"
            } else if a.switches.contains("silhouette") {
                "variant A' (silhouette outline)"
            } else {
                "variant B (no outline)"
            },
            if dark { "dark" } else { "light" },
            if shadow { ", web drop-shadow" } else { "" },
            if scale != 1.0 { format!(", rendered at {scale}x") } else { String::new() }
        )
    });
    svg.push_str(&format!(
        r#"<text x="{margin}" y="28" font-family="DejaVu Sans" font-size="16" font-weight="bold" fill="{caption}">{}</text>"#,
        xml_escape(&title)
    ));
    for (i, f) in files.iter().enumerate() {
        let text = apply_variant(fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?, &a)?;
        let id = f.file_stem().unwrap_or_default().to_string_lossy().to_string();
        let kind = if f.components().any(|c| c.as_os_str() == "sections") { "section" } else { "item" };
        let label = names.get(&format!("{kind}:{id}")).cloned().unwrap_or_else(|| id.clone());
        let gx = margin + (i % cols) as f32 * (group_w + group_gap);
        let gy = head + margin + (i / cols) as f32 * row_h;
        for (t, (color, _)) in tiles.iter().enumerate() {
            let x = gx + t as f32 * (TILE + gap);
            svg.push_str(&format!(r#"<rect x="{x}" y="{gy}" width="{TILE}" height="{TILE}" rx="8" fill="{color}"/>"#));
            let icon = nested_icon(&text, &format!("i{i}t{t}"), x + (TILE - ICON) / 2.0, gy + 8.0, ICON)?;
            if shadow {
                svg.push_str(&format!(r#"<g filter="url(#ds)">{icon}</g>"#));
            } else {
                svg.push_str(&icon);
            }
            // The real tile autosizes between 14 and 12 px and ellipsizes; approximate that.
            let fs = if label.chars().count() > 13 { 12 } else { 14 };
            let label: String = if label.chars().count() > 15 {
                label.chars().take(14).collect::<String>() + "…"
            } else {
                label.clone()
            };
            svg.push_str(&format!(
                r##"<text x="{}" y="{}" text-anchor="middle" font-family="DejaVu Sans" font-size="{fs}" font-weight="bold" fill="#ffffff">{}</text>"##,
                x + TILE / 2.0,
                gy + 8.0 + ICON + 20.0,
                xml_escape(&label)
            ));
        }
        svg.push_str(&format!(
            r#"<text x="{gx}" y="{}" font-family="DejaVu Sans Mono" font-size="11" fill="{caption}">{kind}/{}</text>"#,
            gy + TILE + 14.0,
            xml_escape(&id)
        ));
    }
    svg.push_str("</svg>");

    let opt = font_options();
    let tree = parse_tree(&svg, &opt)?;
    let pixmap = render_tree(&tree, scale, None)?;
    pixmap.save_png(out).map_err(|e| format!("{out}: {e}"))?;
    println!("{out}: {} icons, {}x{} px", files.len(), pixmap.width(), pixmap.height());
    Ok(ExitCode::SUCCESS)
}

// ---------- contrast (silhouette edge vs tile) ----------

fn srgb_to_lab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Share of silhouette-edge pixels that are hard to tell from the tile (CIE76 delta E below `WEAK_DE`) and the
/// median delta E, for one icon composited on one tile colour.
const WEAK_DE: f32 = 25.0;

fn rel_luminance(c: [f32; 3]) -> f32 {
    let lin = |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// Returns (weak-edge %, median delta E, median luminance contrast ratio).
fn edge_stats(pixmap: &tiny_skia::Pixmap, tile: [f32; 3]) -> (f32, f32, f32) {
    let (w, h) = (pixmap.width() as i32, pixmap.height() as i32);
    let px = pixmap.pixels();
    let alpha = |x: i32, y: i32| -> f32 {
        if x < 0 || y < 0 || x >= w || y >= h { 0.0 } else { px[(y * w + x) as usize].alpha() as f32 / 255.0 }
    };
    let tile_lab = srgb_to_lab(tile[0], tile[1], tile[2]);
    let tile_l = rel_luminance(tile);
    let mut des = vec![];
    let mut ratios = vec![];
    for y in 0..h {
        for x in 0..w {
            // The outermost opaque ring of the silhouette (anti-aliased pixels would halve every difference).
            let a = alpha(x, y);
            if a < 0.95 {
                continue;
            }
            let outside = [(-1, 0), (1, 0), (0, -1), (0, 1), (-2, 0), (2, 0), (0, -2), (0, 2)]
                .iter()
                .any(|(dx, dy)| alpha(x + dx, y + dy) < 0.5);
            if !outside {
                continue;
            }
            let p = px[(y * w + x) as usize].demultiply();
            let c = |v: u8, t: f32| (v as f32 / 255.0) * a + t * (1.0 - a);
            let rgb = [c(p.red(), tile[0]), c(p.green(), tile[1]), c(p.blue(), tile[2])];
            let l = rel_luminance(rgb);
            ratios.push((l.max(tile_l) + 0.05) / (l.min(tile_l) + 0.05));
            let lab = srgb_to_lab(rgb[0], rgb[1], rgb[2]);
            let de = ((lab[0] - tile_lab[0]).powi(2) + (lab[1] - tile_lab[1]).powi(2) + (lab[2] - tile_lab[2]).powi(2)).sqrt();
            des.push(de);
        }
    }
    if des.is_empty() {
        return (0.0, 0.0, 1.0);
    }
    des.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ratios.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let weak = des.iter().filter(|d| **d < WEAK_DE).count() as f32 / des.len() as f32;
    (weak * 100.0, des[des.len() / 2], ratios[ratios.len() / 2])
}

fn cmd_contrast(args: &[String]) -> Result<ExitCode, String> {
    let a = parse_args(args, &["size", "tiles", "max-weak"])?;
    let max_weak: f32 = a.flags.get("max-weak").map(|s| s.parse().map_err(|_| "bad --max-weak")).transpose()?.unwrap_or(25.0);
    let size: f32 = a.flags.get("size").map(|s| s.parse().map_err(|_| "bad --size")).transpose()?.unwrap_or(58.0);
    let tiles: Vec<String> = a
        .flags
        .get("tiles")
        .map(|t| t.split(',').map(String::from).collect())
        .unwrap_or_else(|| vec!["#f16a6f".into(), "#6eb8ae".into(), "#ff3b6b".into(), "#14cfc1".into()]);
    let tile_rgb: Vec<[f32; 3]> = tiles
        .iter()
        .map(|t| parse_color(t).map(|c| [c.red(), c.green(), c.blue()]))
        .collect::<Result<_, _>>()?;
    let files = collect_svgs(&a.positional)?;
    let opt = usvg::Options::default();
    print!("{:<22}", "icon: weak%/dE/CR");
    for t in &tiles {
        print!(" {t:>18}");
    }
    println!();
    let mut sums = vec![0.0f32; tiles.len()];
    let mut de_sums = vec![0.0f32; tiles.len()];
    let mut cr_sums = vec![0.0f32; tiles.len()];
    let mut worst: Vec<(f32, String)> = vec![];
    for f in &files {
        let text = apply_variant(fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?, &a)?;
        let tree = parse_tree(&text, &opt)?;
        let pixmap = render_tree(&tree, size / 64.0, None)?;
        let id = f.file_stem().unwrap_or_default().to_string_lossy().to_string();
        print!("{id:<22}");
        for (i, t) in tile_rgb.iter().enumerate() {
            let (weak, median, cr) = edge_stats(&pixmap, *t);
            sums[i] += weak;
            de_sums[i] += median;
            cr_sums[i] += cr;
            worst.push((weak, format!("{id} on {}", tiles[i])));
            print!(" {:>6.1}% {:>4.0} {:>4.2}", weak, median, cr);
        }
        println!();
    }
    let n = files.len() as f32;
    print!("{:<22}", "MEAN");
    for i in 0..tiles.len() {
        print!(" {:>6.1}% {:>4.0} {:>4.2}", sums[i] / n, de_sums[i] / n, cr_sums[i] / n);
    }
    println!();
    worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("worst: {}", worst.iter().take(8).map(|(w, n)| format!("{n} {w:.0}%")).collect::<Vec<_>>().join("; "));
    let failing: Vec<&String> = worst.iter().filter(|(w, _)| *w > max_weak).map(|(_, n)| n).collect();
    if failing.is_empty() {
        println!("gate: every icon <= {max_weak}% weak edge");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("gate FAILED (> {max_weak}% weak edge): {}", failing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("; "));
        Ok(ExitCode::from(1))
    }
}
