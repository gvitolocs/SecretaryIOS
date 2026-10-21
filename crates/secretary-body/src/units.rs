//! Fixed-point weight parsing and integer label formatting (no floats).

/// Parse a non-negative decimal with up to three decimals (`,` or `.`) into
/// thousandths. Same rules as `secretary_gym::decimal_milli`.
pub fn decimal_milli(input: &str) -> Result<u64, String> {
    let normalized = input.trim().replace(',', ".");
    let parts: Vec<_> = normalized.split('.').collect();
    if parts.is_empty()
        || parts.len() > 2
        || parts[0].is_empty()
        || !parts[0].bytes().all(|b| b.is_ascii_digit())
    {
        return Err("Enter a positive number with up to three decimal places".into());
    }
    let fraction = parts.get(1).copied().unwrap_or("");
    if fraction.len() > 3 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Use at most three decimal places".into());
    }
    let whole = parts[0].parse::<u64>().map_err(|_| "Number is too large")?;
    let fractional = format!("{fraction:0<3}")
        .parse::<u64>()
        .map_err(|_| "Invalid decimal")?;
    whole
        .checked_mul(1000)
        .and_then(|n| n.checked_add(fractional))
        .ok_or_else(|| "Number is too large".into())
}

/// A weight string in `kg` or `lb` → grams (1 lb = 453.59237 g, rounded to
/// the nearest gram). Same arithmetic as `secretary_gym::parse_weight`.
pub fn parse_weight(input: &str, unit: &str) -> Result<u64, String> {
    let value = decimal_milli(input)?;
    let grams = match unit {
        "kg" => value,
        "lb" => ((value as u128 * 45_359_237 + 50_000_000) / 100_000_000)
            .try_into()
            .map_err(|_| "Weight is too large")?,
        _ => return Err("Weight unit must be kg or lb".into()),
    };
    if grams > 2_000_000 {
        return Err("Weight exceeds 2,000 kg".into());
    }
    Ok(grams)
}

/// Tenths of the display unit, rounded half up.
pub fn tenths(grams: u64, unit: &str) -> u64 {
    if unit == "lb" {
        ((grams as u128 * 1_000_000 + 22_679_618) / 45_359_237) as u64
    } else {
        (grams + 50) / 100
    }
}

/// `80500, "kg"` → `"80.5 kg"`.
pub fn label(grams: u64, unit: &str) -> String {
    let t = tenths(grams, unit);
    format!("{}.{} {unit}", t / 10, t % 10)
}

/// `-400, "kg"` → `"-0.4 kg"`, `1200` → `"+1.2 kg"`, rounding to zero → `"0.0 kg"`.
pub fn signed_label(delta: i64, unit: &str) -> String {
    let t = tenths(delta.unsigned_abs(), unit);
    let sign = match (delta.signum(), t) {
        (_, 0) | (0, _) => "",
        (1, _) => "+",
        _ => "-",
    };
    format!("{sign}{}.{} {unit}", t / 10, t % 10)
}
