//! `xlsx-tool`: the crate from the command line, and the round-trip proof (docs/xlsx.md).
//!
//! ```text
//! xlsx-tool json <file>                      the view the apps get
//! xlsx-tool timesheet <file>                 the AU worktime form and totals
//! xlsx-tool edit <in> <out> 'Sheet!A1=text' … typed input, as in the app (`=` empty clears)
//! xlsx-tool diff <before> <after>            structural diff of every part
//! ```

use std::collections::BTreeMap;
use std::process::ExitCode;

use secretary_xlsx::package::Package;
use secretary_xlsx::xml::{attr, local, Scanner, Tok};
use secretary_xlsx::{apply, read, Edit, EditValue, Options};

fn main() -> ExitCode {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let r = match a.first().map(String::as_str) {
        Some("json") if a.len() == 2 => json(&a[1], false),
        Some("timesheet") if a.len() == 2 => json(&a[1], true),
        Some("edit") if a.len() >= 4 => edit(&a[1], &a[2], &a[3..]),
        Some("diff") if a.len() == 3 => diff(&a[1], &a[2]),
        _ => Err("usage: xlsx-tool json|timesheet <file> | edit <in> <out> 'Sheet!A1=value'… | diff <a> <b>".into()),
    };
    match r {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(2)
        }
    }
}

fn load(p: &str) -> Result<Vec<u8>, String> {
    std::fs::read(p).map_err(|e| format!("{p}: {e}"))
}

fn json(p: &str, ts: bool) -> Result<ExitCode, String> {
    let wb = read(&load(p)?).map_err(|e| e.to_string())?;
    let mut v = secretary_xlsx::view::workbook_json(&wb);
    if ts {
        v = v["timesheet"].take();
    }
    println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    Ok(ExitCode::SUCCESS)
}

fn edit(src: &str, dst: &str, specs: &[String]) -> Result<ExitCode, String> {
    let mut edits = Vec::new();
    for s in specs {
        let (target, value) = s
            .split_once('=')
            .ok_or_else(|| format!("{s}: expected Sheet!A1=value"))?;
        let (sheet, cell) = target
            .rsplit_once('!')
            .ok_or_else(|| format!("{s}: expected Sheet!A1=value"))?;
        edits.push(Edit {
            sheet: sheet.trim_matches('\'').to_string(),
            cell: cell.to_string(),
            value: if value.is_empty() {
                EditValue::Empty
            } else {
                EditValue::Input(value.to_string())
            },
        });
    }
    let out = apply(&load(src)?, &edits, &Options::default())
        .map_err(|e| format!("{}: {e}", e.code()))?;
    std::fs::write(dst, &out.bytes).map_err(|e| format!("{dst}: {e}"))?;
    for c in &out.changed {
        println!("{}!{}: {:?} -> {:?}", c.sheet, c.cell, c.before, c.after);
    }
    println!("parts rewritten: {}", out.parts.join(", "));
    Ok(ExitCode::SUCCESS)
}

/// A sheet part with every `<c>` element replaced by its reference, and the elements by ref.
fn split_cells(text: &str) -> (String, BTreeMap<String, String>) {
    let mut skeleton = String::with_capacity(text.len());
    let mut cells = BTreeMap::new();
    let mut sc = Scanner::new(text);
    let mut cursor = 0;
    while let Some(Ok(t)) = sc.next() {
        if let Tok::Open {
            name,
            attrs,
            empty,
            start,
            end,
        } = t
        {
            if local(name) != "c" {
                continue;
            }
            let mut cend = end;
            if !empty {
                let mut depth = 0;
                while let Some(Ok(t)) = sc.next() {
                    match t {
                        Tok::Open { empty: false, .. } => depth += 1,
                        Tok::Close { name, end, .. } => {
                            if depth == 0 && local(name) == "c" {
                                cend = end;
                                break;
                            }
                            depth -= 1;
                        }
                        _ => {}
                    }
                }
            }
            let r = attr(attrs, "r").unwrap_or_default();
            skeleton.push_str(&text[cursor..start]);
            skeleton.push_str(&format!("\u{1}{r}\u{1}"));
            cells.insert(r, text[start..cend].to_string());
            cursor = cend;
        }
    }
    skeleton.push_str(&text[cursor..]);
    (skeleton, cells)
}

fn first_difference(a: &str, b: &str) -> String {
    let p = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    let from = a[..p].char_indices().rev().nth(80).map_or(0, |(i, _)| i);
    let ea = a[p..]
        .char_indices()
        .nth(120)
        .map_or(a.len(), |(i, _)| p + i);
    let eb = b[p..]
        .char_indices()
        .nth(120)
        .map_or(b.len(), |(i, _)| p + i);
    format!(
        "    before: …{}\n    after:  …{}",
        &a[from..ea],
        &b[from..eb]
    )
}

fn diff(pa: &str, pb: &str) -> Result<ExitCode, String> {
    let (ba, bb) = (load(pa)?, load(pb)?);
    let a = Package::open(&ba).map_err(|e| e.to_string())?;
    let b = Package::open(&bb).map_err(|e| e.to_string())?;
    let mut same = 0;
    let mut problems = 0;
    if a.names() != b.names() {
        println!("part list or order differs");
        for n in a.names().iter().filter(|n| !b.has(n)) {
            println!("  only before: {n}");
        }
        for n in b.names().iter().filter(|n| !a.has(n)) {
            println!("  only after:  {n}");
        }
        problems += 1;
    }
    for n in a.names().iter().filter(|n| b.has(n)) {
        let (x, y) = (
            a.read(n).map_err(|e| e.to_string())?,
            b.read(n).map_err(|e| e.to_string())?,
        );
        if x == y {
            same += 1;
            continue;
        }
        let (x, y) = (
            String::from_utf8_lossy(&x.unwrap_or_default()).into_owned(),
            String::from_utf8_lossy(&y.unwrap_or_default()).into_owned(),
        );
        if n.contains("worksheets/") {
            let ((sa, ca), (sb, cb)) = (split_cells(&x), split_cells(&y));
            let changed: Vec<_> = cb.iter().filter(|(r, c)| ca.get(*r) != Some(*c)).collect();
            let removed: Vec<_> = ca.keys().filter(|r| !cb.contains_key(*r)).collect();
            println!(
                "{n}: {} cell(s) changed, {} removed, rest of the part identical: {}",
                changed.len(),
                removed.len(),
                sa == sb
            );
            for (r, c) in &changed {
                println!(
                    "    {r}: {} -> {c}",
                    ca.get(*r).map_or("(none)", String::as_str)
                );
            }
            if sa != sb {
                // Inserted cells change the skeleton too; show where.
                println!("{}", first_difference(&sa, &sb));
            }
            if !removed.is_empty() {
                problems += 1;
            }
        } else {
            let tag = |s: &str| {
                Scanner::new(s)
                    .flatten()
                    .find_map(|t| match t {
                        Tok::Open {
                            name, start, end, ..
                        } if local(name) == "calcPr" => Some(s[start..end].to_string()),
                        _ => None,
                    })
                    .unwrap_or_default()
            };
            let (ta, tb) = (tag(&x), tag(&y));
            let rest_same = !ta.is_empty() && x.replacen(&ta, "", 1) == y.replacen(&tb, "", 1);
            println!("{n}: differs; only calcPr: {rest_same}");
            if rest_same {
                println!("    {ta} -> {tb}");
            } else {
                println!("{}", first_difference(&x, &y));
                problems += 1;
            }
        }
    }
    println!("{same} part(s) byte-identical");
    Ok(if problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
