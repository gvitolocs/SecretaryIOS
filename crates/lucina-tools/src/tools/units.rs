//! `unit_convert`: length, mass, volume, area, speed, time, data, energy,
//! pressure and temperature.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use super::calculator::format_number;
use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

/// `(category, factor to the base unit, names)`. Temperature is special.
const UNITS: &[(&str, f64, &[&str])] = &[
    (
        "length",
        0.001,
        &[
            "mm",
            "millimeter",
            "millimeters",
            "millimetre",
            "millimetri",
            "millimetro",
        ],
    ),
    (
        "length",
        0.01,
        &[
            "cm",
            "centimeter",
            "centimeters",
            "centimetre",
            "centimetri",
            "centimetro",
        ],
    ),
    (
        "length",
        1.0,
        &["m", "meter", "meters", "metre", "metres", "metri", "metro"],
    ),
    (
        "length",
        1000.0,
        &[
            "km",
            "kilometer",
            "kilometers",
            "kilometre",
            "kilometres",
            "chilometri",
            "chilometro",
        ],
    ),
    (
        "length",
        0.0254,
        &["in", "inch", "inches", "pollici", "pollice", "\""],
    ),
    (
        "length",
        0.3048,
        &["ft", "foot", "feet", "piedi", "piede", "'"],
    ),
    ("length", 0.9144, &["yd", "yard", "yards", "iarde"]),
    (
        "length",
        1609.344,
        &["mi", "mile", "miles", "miglia", "miglio"],
    ),
    (
        "length",
        1852.0,
        &["nmi", "nautical mile", "nautical miles", "miglia nautiche"],
    ),
    (
        "mass",
        0.000001,
        &["mg", "milligram", "milligrams", "milligrammi"],
    ),
    ("mass", 0.001, &["g", "gram", "grams", "grammi", "grammo"]),
    (
        "mass",
        1.0,
        &[
            "kg",
            "kilo",
            "kilos",
            "kilogram",
            "kilograms",
            "chili",
            "chilo",
            "chilogrammi",
        ],
    ),
    (
        "mass",
        1000.0,
        &["t", "tonne", "tonnes", "ton", "tons", "tonnellate"],
    ),
    (
        "mass",
        0.028349523125,
        &["oz", "ounce", "ounces", "once", "oncia"],
    ),
    (
        "mass",
        0.45359237,
        &["lb", "lbs", "pound", "pounds", "libbre", "libbra"],
    ),
    ("mass", 6.35029318, &["st", "stone", "stones"]),
    (
        "volume",
        0.001,
        &[
            "ml",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitri",
        ],
    ),
    ("volume", 0.01, &["cl", "centiliter", "centilitri"]),
    ("volume", 0.1, &["dl", "deciliter", "decilitri"]),
    (
        "volume",
        1.0,
        &["l", "liter", "liters", "litre", "litres", "litri", "litro"],
    ),
    (
        "volume",
        1000.0,
        &["m3", "cubic meter", "cubic meters", "metri cubi"],
    ),
    (
        "volume",
        0.00492892159375,
        &["tsp", "teaspoon", "teaspoons", "cucchiaino", "cucchiaini"],
    ),
    (
        "volume",
        0.01478676478125,
        &["tbsp", "tablespoon", "tablespoons", "cucchiaio", "cucchiai"],
    ),
    ("volume", 0.2365882365, &["cup", "cups", "tazza", "tazze"]),
    (
        "volume",
        0.0295735295625,
        &["floz", "fl oz", "fluid ounce", "fluid ounces"],
    ),
    (
        "volume",
        0.473176473,
        &["pt", "pint", "pints", "pinta", "pinte"],
    ),
    ("volume", 0.946352946, &["qt", "quart", "quarts"]),
    (
        "volume",
        3.785411784,
        &["gal", "gallon", "gallons", "galloni", "gallone"],
    ),
    (
        "area",
        1.0,
        &[
            "m2",
            "m²",
            "sqm",
            "square meter",
            "square meters",
            "metri quadri",
            "mq",
        ],
    ),
    (
        "area",
        1_000_000.0,
        &[
            "km2",
            "km²",
            "square kilometer",
            "square kilometers",
            "chilometri quadri",
        ],
    ),
    (
        "area",
        10_000.0,
        &["ha", "hectare", "hectares", "ettari", "ettaro"],
    ),
    ("area", 4046.8564224, &["acre", "acres", "acri"]),
    (
        "area",
        0.09290304,
        &["ft2", "ft²", "sqft", "square foot", "square feet"],
    ),
    (
        "speed",
        1.0,
        &["m/s", "mps", "meters per second", "metri al secondo"],
    ),
    (
        "speed",
        1.0 / 3.6,
        &[
            "km/h",
            "kmh",
            "kph",
            "kilometers per hour",
            "chilometri orari",
        ],
    ),
    ("speed", 0.44704, &["mph", "miles per hour"]),
    (
        "speed",
        0.514444444,
        &["kn", "kt", "knot", "knots", "nodi", "nodo"],
    ),
    (
        "time",
        1.0,
        &[
            "s", "sec", "secs", "second", "seconds", "secondi", "secondo",
        ],
    ),
    (
        "time",
        60.0,
        &["min", "mins", "minute", "minutes", "minuti", "minuto"],
    ),
    (
        "time",
        3600.0,
        &["h", "hr", "hrs", "hour", "hours", "ore", "ora"],
    ),
    ("time", 86400.0, &["d", "day", "days", "giorni", "giorno"]),
    (
        "time",
        604800.0,
        &["wk", "week", "weeks", "settimane", "settimana"],
    ),
    ("data", 1.0, &["b", "byte", "bytes"]),
    ("data", 1e3, &["kb", "kilobyte", "kilobytes"]),
    ("data", 1e6, &["mb", "megabyte", "megabytes"]),
    ("data", 1e9, &["gb", "gigabyte", "gigabytes"]),
    ("data", 1e12, &["tb", "terabyte", "terabytes"]),
    ("data", 1024.0, &["kib", "kibibyte"]),
    ("data", 1_048_576.0, &["mib", "mebibyte"]),
    ("data", 1_073_741_824.0, &["gib", "gibibyte"]),
    ("energy", 1.0, &["j", "joule", "joules"]),
    ("energy", 1000.0, &["kj", "kilojoule", "kilojoules"]),
    (
        "energy",
        4.184,
        &["cal", "calorie", "calories", "calorie piccole"],
    ),
    (
        "energy",
        4184.0,
        &[
            "kcal",
            "kilocalorie",
            "kilocalories",
            "chilocalorie",
            "calorie alimentari",
        ],
    ),
    ("energy", 3600.0, &["wh", "watt hour", "watt hours"]),
    (
        "energy",
        3_600_000.0,
        &["kwh", "kilowatt hour", "kilowatt hours", "chilowattora"],
    ),
    ("pressure", 1.0, &["pa", "pascal"]),
    (
        "pressure",
        100.0,
        &["hpa", "hectopascal", "mbar", "millibar"],
    ),
    ("pressure", 1000.0, &["kpa", "kilopascal"]),
    ("pressure", 100_000.0, &["bar"]),
    ("pressure", 6894.757293168, &["psi"]),
    (
        "pressure",
        101_325.0,
        &["atm", "atmosphere", "atmosphere", "atmosfere"],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Unit {
    Linear(&'static str, f64, &'static str),
    Temp(char),
}

fn lookup(raw: &str) -> Option<Unit> {
    let s = raw.trim().to_lowercase();
    let s = s.trim_end_matches('.');
    match s {
        "c" | "°c" | "celsius" | "centigradi" | "gradi" | "gradi celsius" | "ºc" => {
            return Some(Unit::Temp('c'))
        }
        "f" | "°f" | "fahrenheit" | "gradi fahrenheit" | "ºf" => return Some(Unit::Temp('f')),
        "k" | "kelvin" => return Some(Unit::Temp('k')),
        _ => {}
    }
    UNITS
        .iter()
        .find(|(_, _, names)| names.contains(&s))
        .map(|(cat, f, names)| Unit::Linear(cat, *f, names[0]))
}

fn to_celsius(v: f64, u: char) -> f64 {
    match u {
        'f' => (v - 32.0) * 5.0 / 9.0,
        'k' => v - 273.15,
        _ => v,
    }
}

fn from_celsius(c: f64, u: char) -> f64 {
    match u {
        'f' => c * 9.0 / 5.0 + 32.0,
        'k' => c + 273.15,
        _ => c,
    }
}

/// Convert, returning `(result, from canonical name, to canonical name)`.
pub fn convert(value: f64, from: &str, to: &str) -> Result<(f64, String, String), String> {
    let f = lookup(from).ok_or_else(|| format!("unknown unit {from:?}"))?;
    let t = lookup(to).ok_or_else(|| format!("unknown unit {to:?}"))?;
    match (f, t) {
        (Unit::Temp(a), Unit::Temp(b)) => {
            let name = |u: char| match u {
                'c' => "°C",
                'f' => "°F",
                _ => "K",
            };
            Ok((
                from_celsius(to_celsius(value, a), b),
                name(a).into(),
                name(b).into(),
            ))
        }
        (Unit::Linear(ca, fa, na), Unit::Linear(cb, fb, nb)) if ca == cb => {
            Ok((value * fa / fb, na.into(), nb.into()))
        }
        _ => Err(format!(
            "cannot convert {from} to {to}: different quantities"
        )),
    }
}

#[derive(Deserialize, JsonSchema)]
struct Args {
    value: f64,
    /// Source unit, e.g. "km", "lb", "°F", "cup".
    from: String,
    /// Target unit.
    to: String,
}

pub struct UnitConvertTool;

#[async_trait::async_trait]
impl Tool for UnitConvertTool {
    fn name(&self) -> &'static str {
        "unit_convert"
    }
    fn description(&self) -> &'static str {
        "Convert a value between units of length, mass, volume, area, speed, time, data, energy, pressure or temperature (°C/°F/K)."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, _ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let (result, from, to) = convert(a.value, &a.from, &a.to).map_err(ToolError::new)?;
        Ok(
            json!({"value": a.value, "from": from, "to": to, "result": result, "formatted": format!("{} {to}", format_number(result))}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts() {
        let close = |a: f64, b: f64| (a - b).abs() < 1e-6;
        assert!(close(convert(10.0, "km", "mi").unwrap().0, 6.213711922));
        assert!(close(convert(100.0, "°F", "C").unwrap().0, 37.777777778));
        assert!(close(convert(0.0, "celsius", "kelvin").unwrap().0, 273.15));
        assert!(close(convert(2.0, "libbre", "kg").unwrap().0, 0.90718474));
        assert!(close(convert(1.0, "cup", "ml").unwrap().0, 236.5882365));
        assert!(close(convert(90.0, "minuti", "ore").unwrap().0, 1.5));
        assert!(close(
            convert(100.0, "km/h", "m/s").unwrap().0,
            27.777777778
        ));
        assert!(convert(1.0, "kg", "m").is_err());
        assert!(convert(1.0, "parsec", "m").is_err());
    }
}
