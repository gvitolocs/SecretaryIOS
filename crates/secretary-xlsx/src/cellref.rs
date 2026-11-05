//! A1 references: `B12` is row 12, column 2 (both 1-based).

pub const MAX_ROW: u32 = 1_048_576;
pub const MAX_COL: u32 = 16_384;

/// Column number to letters: 1 → `A`, 27 → `AA`.
pub fn col_name(mut col: u32) -> String {
    let mut out = Vec::new();
    while col > 0 {
        let r = (col - 1) % 26;
        out.push(b'A' + r as u8);
        col = (col - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// `(row, col)` of `B12` or `$B$12`; `None` when it is not a cell inside the sheet's limits.
pub fn parse(r: &str) -> Option<(u32, u32)> {
    let b = r.as_bytes();
    let mut i = 0;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let mut col: u32 = 0;
    let cs = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        col = col.checked_mul(26)? + (b[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
        if i - cs > 3 {
            return None;
        }
    }
    if i == cs {
        return None;
    }
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let digits = &r[i..];
    if digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_digit()) || digits.starts_with('0') {
        return None;
    }
    let row: u32 = digits.parse().ok()?;
    (row <= MAX_ROW && col <= MAX_COL).then_some((row, col))
}

pub fn name(row: u32, col: u32) -> String {
    format!("{}{row}", col_name(col))
}

/// A rectangle `(r1, c1, r2, c2)` from `A1:B3` or a single `A1`.
pub fn parse_range(r: &str) -> Option<(u32, u32, u32, u32)> {
    match r.split_once(':') {
        Some((a, b)) => {
            let (r1, c1) = parse(a)?;
            let (r2, c2) = parse(b)?;
            Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
        }
        None => parse(r).map(|(r, c)| (r, c, r, c)),
    }
}

/// Whether `(row, col)` falls in a space-separated `sqref` list.
pub fn in_sqref(sqref: &str, row: u32, col: u32) -> bool {
    sqref.split_whitespace().any(|part| {
        parse_range(part)
            .is_some_and(|(r1, c1, r2, c2)| (r1..=r2).contains(&row) && (c1..=c2).contains(&col))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for c in [1, 26, 27, 52, 53, 702, 703, 16384] {
            let n = col_name(c);
            assert_eq!(parse(&format!("{n}1")), Some((1, c)), "{n}");
        }
        assert_eq!(col_name(45), "AS");
        assert_eq!(parse("$M$3"), Some((3, 13)));
        assert_eq!(parse("A0"), None);
        assert_eq!(parse("XFE1"), None);
        assert_eq!(parse("1A"), None);
        assert_eq!(parse_range("F38:F9"), Some((9, 6, 38, 6)));
        assert!(in_sqref("A1 I9:K38", 20, 10));
        assert!(!in_sqref("I9:K38", 20, 12));
    }
}
