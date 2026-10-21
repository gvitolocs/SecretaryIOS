//! Canonical grocery products (`src/grocery/products.js`).

#[derive(Debug, Clone, PartialEq)]
pub struct Product {
    pub id: String,
    pub name: String,
    pub category: String,
    pub aliases: Vec<String>,
    pub confidence: f64,
}

const PRODUCT_ALIASES: &[(&str, &str, &str, &[&str])] = &[
    (
        "chicken_breast",
        "chicken breast",
        "meat",
        &[
            "chicken breast",
            "chicken fillet",
            "kyllingebryst",
            "kyllinge brystfilet",
        ],
    ),
    ("chicken", "chicken", "meat", &["chicken", "kylling"]),
    ("mozzarella", "mozzarella", "dairy", &["mozzarella"]),
    ("ham", "ham", "meat", &["ham", "prosciutto cotto", "skinke"]),
    (
        "prosciutto",
        "prosciutto",
        "meat",
        &["prosciutto", "parma ham", "parmaskinke"],
    ),
    (
        "pasta",
        "pasta",
        "pasta/rice",
        &["pasta", "spaghetti", "penne"],
    ),
    ("rice", "rice", "pasta/rice", &["rice", "ris"]),
    ("milk", "milk", "dairy", &["milk", "latte", "mælk"]),
    ("yogurt", "yogurt", "dairy", &["yogurt", "yoghurt"]),
    (
        "rye_bread",
        "rye bread",
        "bakery",
        &["rye bread", "pane di segale", "rugbrød"],
    ),
    ("eggs", "eggs", "dairy", &["egg", "eggs", "æg"]),
];

/// Lowercase; runs of non-letters/digits become one space; trimmed.
pub fn normalize_product_name(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut gap = false;
    for c in lower.chars() {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

fn product(entry: &(&str, &str, &str, &[&str]), confidence: f64) -> Product {
    Product {
        id: entry.0.into(),
        name: entry.1.into(),
        category: entry.2.into(),
        aliases: entry.3.iter().map(|s| (*s).to_owned()).collect(),
        confidence,
    }
}

pub fn resolve_canonical_product(raw: &str) -> Product {
    let query = normalize_product_name(raw);
    if let Some(e) = PRODUCT_ALIASES
        .iter()
        .find(|e| e.3.iter().any(|a| normalize_product_name(a) == query))
    {
        return product(e, 1.0);
    }
    // Aliases must start at a word boundary so "ham" skips "shampoo".
    let padded = format!(" {query}");
    if let Some(e) = PRODUCT_ALIASES.iter().find(|e| {
        e.3.iter()
            .any(|a| padded.contains(&format!(" {}", normalize_product_name(a))))
    }) {
        return product(e, 0.86);
    }
    let id: String = query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
        .take(72)
        .collect();
    let name = raw.trim().to_owned();
    Product {
        id: if id.is_empty() {
            "unknown_product".into()
        } else {
            id
        },
        name: name.clone(),
        category: "other".into(),
        aliases: vec![name],
        confidence: 0.58,
    }
}

/// `(id, displayName, category, aliases)` to seed `grocery.canonical_products`.
pub fn known_canonical_products() -> Vec<Product> {
    PRODUCT_ALIASES.iter().map(|e| product(e, 1.0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_like_node() {
        assert_eq!(resolve_canonical_product("Latte").id, "milk");
        assert_eq!(resolve_canonical_product("latte intero").id, "milk");
        assert_eq!(resolve_canonical_product("shampoo").id, "shampoo");
        assert_eq!(resolve_canonical_product("Pane di segale!").id, "rye_bread");
        let p = resolve_canonical_product("Pomodori San Marzano");
        assert_eq!(
            (p.id.as_str(), p.name.as_str(), p.category.as_str()),
            ("pomodori_san_marzano", "Pomodori San Marzano", "other")
        );
        assert_eq!(resolve_canonical_product("ñoño").id, "oo");
        assert_eq!(resolve_canonical_product("!!!").id, "unknown_product");
    }
}
