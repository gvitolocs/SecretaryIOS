//! vCard (`.vcf`) reader for 2.1, 3.0 and 4.0: names, phones, emails,
//! addresses and the rest of every card in the file.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::Extracted;

const MAX_CONTACTS: usize = 500;
const NOTE_CHARS: usize = 1000;
/// Type tokens that say nothing to a reader.
const NOISE_TYPES: [&str; 3] = ["internet", "x-internet", "voice"];

/// Reads every card (up to 500) in a vCard file.
pub fn parse_vcf(input: &str) -> Extracted {
    let mut out = Extracted::new("contact");
    let mut warnings = Vec::new();
    let mut contacts = Vec::new();
    let mut blocks = Vec::new();
    let mut skipped = 0usize;
    let mut depth = 0usize;
    let mut card: Vec<Prop> = Vec::new();

    for line in logical_lines(input) {
        let Some(prop) = parse_line(&line) else {
            continue;
        };
        match prop.name.as_str() {
            "BEGIN" if prop.value.trim().eq_ignore_ascii_case("VCARD") => {
                if depth == 0 {
                    card.clear();
                }
                depth += 1;
            }
            "END" if prop.value.trim().eq_ignore_ascii_case("VCARD") => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth > 0 {
                    continue;
                }
                if contacts.len() >= MAX_CONTACTS {
                    skipped += 1;
                    continue;
                }
                let contact = build_contact(&card);
                blocks.push(contact.text);
                contacts.push(contact.json);
                if out.title.is_none() {
                    out.title = contact.name;
                }
            }
            // Nested cards (2.1 AGENT) are not read on their own.
            _ if depth == 1 => card.push(prop),
            _ => {}
        }
    }

    if skipped > 0 {
        warnings.push(format!(
            "Only the first {MAX_CONTACTS} contacts were read; {skipped} more were skipped."
        ));
    }
    if contacts.is_empty() {
        warnings.push("No contacts were found in this vCard file.".into());
    }
    let total = contacts.len() + skipped;
    let mut text = Vec::new();
    if total > 1 {
        text.push(format!("{total} contacts."));
    }
    text.extend(blocks);
    out.text = text.join("\n\n");
    out.structured = serde_json::json!({ "contacts": contacts });
    out.warnings = warnings;
    out
}

/// One content line: `group.NAME;PARAMS:VALUE`, value decoded to text
/// (quoted-printable and charset undone, escapes kept for the field parser).
struct Prop {
    group: Option<String>,
    name: String,
    types: Vec<String>,
    value: String,
}

/// Joins folded lines and quoted-printable soft line breaks (`=` at EOL).
fn logical_lines(input: &str) -> Vec<String> {
    let input = input.trim_start_matches('\u{feff}');
    let mut lines: Vec<String> = Vec::new();
    let mut qp_open = false;
    for raw in input.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match lines.last_mut() {
            Some(last) if qp_open => {
                last.pop();
                last.push_str(raw);
            }
            Some(last) if raw.starts_with([' ', '\t']) => {
                last.push_str(&raw[1..]);
            }
            _ if raw.trim().is_empty() => {}
            _ => lines.push(raw.to_string()),
        }
        qp_open = lines
            .last()
            .is_some_and(|l| l.ends_with('=') && is_quoted_printable(l));
    }
    lines
}

fn is_quoted_printable(line: &str) -> bool {
    line.split_once(':')
        .is_some_and(|(head, _)| head.to_ascii_uppercase().contains("QUOTED-PRINTABLE"))
}

fn parse_line(line: &str) -> Option<Prop> {
    let mut in_quotes = false;
    let mut colon = None;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon?;
    let mut parts = split_quoted(&line[..colon], ';').into_iter();
    let full_name = parts.next()?.trim();
    let (group, name) = match full_name.rsplit_once('.') {
        Some((g, n)) => (Some(g.to_string()), n),
        None => (None, full_name),
    };
    let name = name.to_ascii_uppercase();
    if name.is_empty() {
        return None;
    }
    let mut types = Vec::new();
    let mut quoted_printable = false;
    let mut charset = None;
    for p in parts {
        let p = p.trim();
        match p.split_once('=') {
            Some((k, v)) => {
                let v = v.trim().trim_matches('"');
                match k.trim().to_ascii_uppercase().as_str() {
                    "TYPE" => types.extend(
                        v.split(',')
                            .map(|t| t.trim().trim_matches('"').to_ascii_lowercase())
                            .filter(|t| !t.is_empty()),
                    ),
                    "ENCODING" => quoted_printable |= v.eq_ignore_ascii_case("QUOTED-PRINTABLE"),
                    "CHARSET" => charset = Some(v.to_ascii_uppercase()),
                    "PREF" => types.push("pref".into()),
                    _ => {}
                }
            }
            None => match p.to_ascii_uppercase().as_str() {
                "QUOTED-PRINTABLE" => quoted_printable = true,
                "BASE64" | "B" | "8BIT" | "7BIT" | "" => {}
                other => types.push(other.to_ascii_lowercase()),
            },
        }
    }
    let raw = &line[colon + 1..];
    let value = if quoted_printable {
        decode_charset(&qp_decode(raw), charset.as_deref())
    } else {
        raw.to_string()
    };
    Some(Prop {
        group,
        name,
        types,
        value,
    })
}

fn split_quoted(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
        } else if c == sep && !in_quotes {
            parts.push(&s[start..i]);
            start = i + c.len_utf8();
        }
    }
    parts.push(&s[start..]);
    parts
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).and_then(|d| u8::try_from(d).ok())
}

/// `=C3=B2` → bytes; a stray `=` is kept as written.
fn qp_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'=' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn decode_charset(bytes: &[u8], charset: Option<&str>) -> String {
    match charset {
        Some("ISO-8859-1" | "ISO-8859-15" | "LATIN1" | "WINDOWS-1252" | "CP1252") => {
            bytes.iter().map(|&b| char::from(b)).collect()
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Undoes `\n` `\,` `\;` `\:` `\\`.
fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n' | 'N') => out.push('\n'),
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Splits a structured value (`N`, `ADR`, `ORG`) on unescaped `;`.
fn components(v: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ';' => parts.push(unescape(&std::mem::take(&mut cur)).trim().to_string()),
            _ => cur.push(c),
        }
    }
    parts.push(unescape(&cur).trim().to_string());
    parts
}

fn text_value(p: &Prop) -> Option<String> {
    let v = unescape(p.value.trim());
    let v = v.trim();
    (!v.is_empty()).then(|| v.to_string())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// `_$!<Mobile>!$_` (Apple's built-in labels) → `mobile`; custom labels as written.
fn clean_label(label: &str) -> String {
    let l = label.trim();
    match l.strip_prefix("_$!<").and_then(|r| r.strip_suffix(">!$_")) {
        Some(inner) => inner.to_ascii_lowercase(),
        None => l.to_string(),
    }
}

fn types_for(p: &Prop, labels: &HashMap<String, String>) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    let label = p.group.as_ref().and_then(|g| labels.get(g)).cloned();
    for t in p.types.iter().cloned().chain(label) {
        if !NOISE_TYPES.contains(&t.as_str()) && !types.contains(&t) {
            types.push(t);
        }
    }
    types
}

fn put(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(v) = value.filter(|v| !v.is_empty()) {
        map.insert(key.into(), Value::String(v));
    }
}

fn types_json(types: &[String]) -> Value {
    Value::Array(types.iter().cloned().map(Value::String).collect())
}

fn with_types(label: &str, types: &[String]) -> String {
    if types.is_empty() {
        label.to_string()
    } else {
        format!("{label} ({})", types.join(", "))
    }
}

/// `19850412` → `1985-04-12`, `--0412` → `--04-12`; anything else as written.
fn normalize_date(v: &str) -> String {
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if v.len() == 8 && digits(v) {
        return format!("{}-{}-{}", &v[..4], &v[4..6], &v[6..]);
    }
    if let Some(md) = v.strip_prefix("--") {
        if md.len() == 4 && digits(md) {
            return format!("--{}-{}", &md[..2], &md[2..]);
        }
    }
    match v.split_once('T') {
        Some((d, _)) if d.len() == 10 => d.to_string(),
        _ => v.to_string(),
    }
}

struct Contact {
    json: Value,
    text: String,
    name: Option<String>,
}

fn build_contact(props: &[Prop]) -> Contact {
    let labels: HashMap<String, String> = props
        .iter()
        .filter(|p| p.name == "X-ABLABEL")
        .filter_map(|p| Some((p.group.clone()?, clean_label(&unescape(&p.value)))))
        .collect();

    let mut map = Map::new();
    let mut fn_name = None;
    let mut n_parts: Option<Vec<String>> = None;
    let mut org = None;
    let mut title = None;
    let mut nickname = None;
    let mut birthday = None;
    let mut note = None;
    let mut version = None;
    let mut has_photo = false;
    let mut phones = Vec::new();
    let mut emails = Vec::new();
    let mut addresses = Vec::new();
    let mut urls: Vec<String> = Vec::new();
    let mut lines = Vec::new();

    for p in props {
        match p.name.as_str() {
            "VERSION" => version = text_value(p),
            "FN" if fn_name.is_none() => fn_name = text_value(p),
            "N" if n_parts.is_none() => n_parts = Some(components(&p.value)),
            "ORG" if org.is_none() => {
                let parts: Vec<String> = components(&p.value)
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect();
                org = (!parts.is_empty()).then(|| parts.join(", "));
            }
            "TITLE" if title.is_none() => title = text_value(p),
            "NICKNAME" if nickname.is_none() => nickname = text_value(p),
            "BDAY" if birthday.is_none() => birthday = text_value(p).map(|b| normalize_date(&b)),
            "NOTE" if note.is_none() => note = text_value(p).map(|n| clip(&n, NOTE_CHARS)),
            "PHOTO" => has_photo = true,
            "TEL" => {
                let Some(v) = text_value(p) else { continue };
                let number = match v.get(..4) {
                    Some(s) if s.eq_ignore_ascii_case("tel:") => v[4..].to_string(),
                    _ => v,
                };
                let types = types_for(p, &labels);
                lines.push(format!("  {}: {number}", with_types("Phone", &types)));
                phones.push(serde_json::json!({ "number": number, "types": types_json(&types) }));
            }
            "EMAIL" => {
                let Some(v) = text_value(p) else { continue };
                let address = match v.get(..7) {
                    Some(s) if s.eq_ignore_ascii_case("mailto:") => v[7..].to_string(),
                    _ => v,
                };
                let types = types_for(p, &labels);
                lines.push(format!("  {}: {address}", with_types("Email", &types)));
                emails.push(serde_json::json!({ "address": address, "types": types_json(&types) }));
            }
            "ADR" => {
                let c = components(&p.value);
                let get = |i: usize| c.get(i).cloned().unwrap_or_default();
                let street: Vec<String> = [get(0), get(1), get(2)]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect();
                let street = street.join(", ").replace('\n', ", ");
                let fields = [street, get(3), get(4), get(5), get(6)];
                if fields.iter().all(String::is_empty) {
                    continue;
                }
                let types = types_for(p, &labels);
                let mut a = Map::new();
                for (key, val) in ["street", "city", "region", "postcode", "country"]
                    .into_iter()
                    .zip(fields.iter())
                {
                    put(&mut a, key, Some(val.clone()));
                }
                a.insert("types".into(), types_json(&types));
                let readable: Vec<&str> = fields
                    .iter()
                    .map(String::as_str)
                    .filter(|s| !s.is_empty())
                    .collect();
                lines.push(format!(
                    "  {}: {}",
                    with_types("Address", &types),
                    readable.join(", ")
                ));
                addresses.push(Value::Object(a));
            }
            "URL" => {
                if let Some(u) = text_value(p) {
                    lines.push(format!("  Web: {u}"));
                    urls.push(u);
                }
            }
            _ => {}
        }
    }

    let full_name = fn_name.or_else(|| {
        let n = n_parts.as_ref()?;
        let get = |i: usize| n.get(i).map(String::as_str).unwrap_or_default();
        let joined: Vec<&str> = [get(3), get(1), get(2), get(0), get(4)]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        (!joined.is_empty()).then(|| joined.join(" "))
    });

    put(&mut map, "full_name", full_name.clone());
    if let Some(n) = &n_parts {
        let mut nm = Map::new();
        for (i, key) in ["family", "given", "additional", "prefix", "suffix"]
            .into_iter()
            .enumerate()
        {
            put(&mut nm, key, n.get(i).cloned());
        }
        map.insert("n".into(), Value::Object(nm));
    }
    put(&mut map, "nickname", nickname.clone());
    put(&mut map, "org", org.clone());
    put(&mut map, "title", title.clone());
    map.insert("phones".into(), Value::Array(phones));
    map.insert("emails".into(), Value::Array(emails));
    map.insert("addresses".into(), Value::Array(addresses));
    map.insert(
        "urls".into(),
        Value::Array(urls.into_iter().map(Value::String).collect()),
    );
    put(&mut map, "birthday", birthday.clone());
    put(&mut map, "note", note.clone());
    map.insert("has_photo".into(), Value::Bool(has_photo));
    put(&mut map, "version", version);

    let mut head: Vec<String> = vec![full_name
        .clone()
        .or_else(|| org.clone())
        .unwrap_or_else(|| "(no name)".into())];
    if let Some(nick) = &nickname {
        head[0].push_str(&format!(" \"{nick}\""));
    }
    if full_name.is_some() {
        head.extend(org);
    }
    head.extend(title);
    let mut text = vec![head.join(" · ")];
    text.extend(lines);
    if let Some(b) = &birthday {
        text.push(format!("  Birthday: {b}"));
    }
    if let Some(n) = &note {
        text.push(format!("  Note: {}", n.replace('\n', "\n        ")));
    }
    Contact {
        json: Value::Object(map),
        text: text.join("\n"),
        name: full_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IOS: &str = include_str!("../../../tests/fixtures/attachments/ios-export.vcf");
    const ANDROID: &str = include_str!("../../../tests/fixtures/attachments/android-export.vcf");

    fn contacts(x: &Extracted) -> &Vec<Value> {
        x.structured["contacts"].as_array().expect("contacts array")
    }

    #[test]
    fn ios_export_with_item_groups() {
        let x = parse_vcf(&IOS.replace('\n', "\r\n"));
        assert_eq!(x.kind, "contact");
        assert!(x.warnings.is_empty(), "{:?}", x.warnings);
        let c = &contacts(&x)[0];
        assert_eq!(x.title.as_deref(), Some("Dott. Giulia Bianchi"));
        assert_eq!(c["n"]["family"], "Bianchi");
        assert_eq!(c["n"]["given"], "Giulia");
        assert_eq!(c["n"]["prefix"], "Dott.");
        assert_eq!(c["org"], "Studio Bianchi, Consulenza");
        assert_eq!(c["title"], "Commercialista");
        assert_eq!(c["version"], "3.0");
        let phones = c["phones"].as_array().expect("phones");
        assert_eq!(phones.len(), 3);
        assert_eq!(phones[0]["number"], "+39 333 123 4567");
        assert_eq!(phones[0]["types"], serde_json::json!(["cell", "pref"]));
        assert_eq!(phones[2]["number"], "+45 20 12 34 56");
        assert_eq!(phones[2]["types"], serde_json::json!(["Copenhagen"]));
        let emails = c["emails"].as_array().expect("emails");
        assert_eq!(emails[0]["address"], "giulia@studiobianchi.example");
        assert_eq!(emails[0]["types"], serde_json::json!(["work", "pref"]));
        let adr = &c["addresses"][0];
        assert_eq!(adr["street"], "Via Monte Napoleone 8");
        assert_eq!(adr["city"], "Milano");
        assert_eq!(adr["region"], "MI");
        assert_eq!(adr["postcode"], "20121");
        assert_eq!(adr["country"], "Italia");
        assert_eq!(adr["types"], serde_json::json!(["work", "pref"]));
        assert_eq!(
            c["urls"],
            serde_json::json!(["https://studiobianchi.example"])
        );
        assert_eq!(c["birthday"], "1988-03-21");
        assert_eq!(
            c["note"],
            "Met at the Copenhagen fair, 2025.\nPrefers WhatsApp."
        );
        assert_eq!(c["has_photo"], true);
        assert!(x
            .text
            .starts_with("Dott. Giulia Bianchi · Studio Bianchi, Consulenza · Commercialista"));
        assert!(x.text.contains("  Phone (Copenhagen): +45 20 12 34 56"));
        assert!(x
            .text
            .contains("  Address (work, pref): Via Monte Napoleone 8, Milano, MI, 20121, Italia"));
        assert!(!x.text.contains("/9j/"));
    }

    #[test]
    fn android_21_quoted_printable() {
        let x = parse_vcf(ANDROID);
        assert!(x.warnings.is_empty(), "{:?}", x.warnings);
        let c = &contacts(&x)[0];
        assert_eq!(x.title.as_deref(), Some("Niccolò Ferrara"));
        assert_eq!(c["n"]["given"], "Niccolò");
        assert_eq!(c["org"], "Pizzeria Da Niccolò");
        let phones = c["phones"].as_array().expect("phones");
        assert_eq!(phones[0]["number"], "+393471234567");
        assert_eq!(phones[0]["types"], serde_json::json!(["cell", "pref"]));
        assert_eq!(phones[1]["types"], serde_json::json!(["home"]));
        assert_eq!(c["emails"][0]["address"], "niccolo@example.it");
        assert_eq!(c["addresses"][0]["street"], "Via dei Tribunali 32");
        assert_eq!(c["addresses"][0]["city"], "Napoli");
        assert_eq!(
            c["note"],
            "Cugino di Marco. Porta il ragù domenica, ricordati di chiedere della casa al mare a Sorrento."
        );
        assert_eq!(c["has_photo"], true);
        // The second card is ISO-8859-1.
        assert_eq!(contacts(&x)[1]["full_name"], "Zoë Ågård");
        assert!(x.text.starts_with("2 contacts."));
    }

    #[test]
    fn vcard_40() {
        let card = "BEGIN:VCARD\nVERSION:4.0\nFN:Mette Sørensen\nN:Sørensen;Mette;;;\nTEL;VALUE=uri;TYPE=\"cell,voice\";PREF=1:tel:+45-20-11-22-33\nEMAIL;TYPE=work:mette@firma.dk\nADR;TYPE=work;LABEL=\"Strandvejen 1\\n2900 Hellerup\":;;Strandvejen 1;Hellerup;;2900;Danmark\nBDAY:19900704\nURL:https://firma.dk\nEND:VCARD\n";
        let x = parse_vcf(card);
        let c = &contacts(&x)[0];
        assert_eq!(c["full_name"], "Mette Sørensen");
        assert_eq!(c["phones"][0]["number"], "+45-20-11-22-33");
        assert_eq!(c["phones"][0]["types"], serde_json::json!(["cell", "pref"]));
        assert_eq!(c["emails"][0]["types"], serde_json::json!(["work"]));
        assert_eq!(c["addresses"][0]["postcode"], "2900");
        assert!(c["addresses"][0].get("region").is_none());
        assert_eq!(c["birthday"], "1990-07-04");
        assert_eq!(c["urls"][0], "https://firma.dk");
        assert_eq!(c["has_photo"], false);
        assert!(x
            .text
            .contains("  Address (work): Strandvejen 1, Hellerup, 2900, Danmark"));
    }

    #[test]
    fn multi_card_file() {
        let file = "BEGIN:VCARD\nVERSION:3.0\nFN:Anna Rossi\nTEL;TYPE=CELL,WORK:+39 320 000 0001\nEND:VCARD\nBEGIN:VCARD\nVERSION:3.0\nN:Verdi;Luca;;Ing.;\nEMAIL:luca@example.com\nEND:VCARD\n\nBEGIN:VCARD\nVERSION:3.0\nORG:ACME A/S\nTEL:+45 70 00 00 00\nEND:VCARD\n";
        let x = parse_vcf(file);
        let cs = contacts(&x);
        assert_eq!(cs.len(), 3);
        assert_eq!(x.title.as_deref(), Some("Anna Rossi"));
        assert_eq!(
            cs[0]["phones"][0]["types"],
            serde_json::json!(["cell", "work"])
        );
        assert_eq!(cs[1]["full_name"], "Ing. Luca Verdi");
        assert!(cs[2].get("full_name").is_none());
        assert!(x.text.starts_with("3 contacts.\n\nAnna Rossi\n"));
        assert!(x.text.contains("\n\nACME A/S\n  Phone: +45 70 00 00 00"));
    }

    #[test]
    fn cap_and_empty() {
        let one = "BEGIN:VCARD\nFN:X\nEND:VCARD\n";
        let x = parse_vcf(&one.repeat(MAX_CONTACTS + 2));
        assert_eq!(contacts(&x).len(), MAX_CONTACTS);
        assert_eq!(x.warnings.len(), 1);
        let empty = parse_vcf("hello");
        assert!(contacts(&empty).is_empty());
        assert_eq!(empty.warnings.len(), 1);
    }

    #[test]
    fn helpers() {
        assert_eq!(qp_decode("Niccol=C3=B2 =ZZ="), "Niccolò =ZZ=".as_bytes());
        assert_eq!(components(r"a\;b;c\,d;;"), vec!["a;b", "c,d", "", ""]);
        assert_eq!(clean_label("_$!<Mobile>!$_"), "mobile");
        assert_eq!(normalize_date("--0412"), "--04-12");
    }
}
