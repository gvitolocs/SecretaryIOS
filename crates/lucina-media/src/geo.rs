//! Offline place names (GeoNames cities via `reverse_geocoder`), so "Copenhagen" works without
//! sending the owner's coordinates anywhere.

use std::sync::OnceLock;

use reverse_geocoder::ReverseGeocoder;

use crate::db::Place;

fn geocoder() -> &'static ReverseGeocoder {
    static G: OnceLock<ReverseGeocoder> = OnceLock::new();
    G.get_or_init(ReverseGeocoder::new)
}

/// Nearest named place for a coordinate. `None` for null-island and out-of-range values.
pub fn place_for(lat: f64, lon: f64) -> Option<Place> {
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || (lat == 0.0 && lon == 0.0) {
        return None;
    }
    let r = geocoder().search((lat, lon)).record;
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    Some(Place {
        name: r.name.clone(),
        admin1: opt(&r.admin1),
        admin2: opt(&r.admin2),
        country: opt(&r.cc),
        lat,
        lon,
    })
}

pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = (lat2 - lat1).to_radians();
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    6371.0 * 2.0 * a.sqrt().atan2((1.0 - a).sqrt())
}

/// English (and for the owner's countries, local) names, so a search for "Denmark" or "Italia"
/// finds photos whose place only carries the ISO code.
pub fn country_name(cc: &str) -> Option<&'static str> {
    Some(match cc {
        "DK" => "Denmark Danmark",
        "IT" => "Italy Italia",
        "SE" => "Sweden Sverige",
        "NO" => "Norway Norge",
        "DE" => "Germany Deutschland",
        "FR" => "France",
        "ES" => "Spain España",
        "PT" => "Portugal",
        "GB" => "United Kingdom UK England",
        "IE" => "Ireland",
        "NL" => "Netherlands Holland",
        "BE" => "Belgium",
        "CH" => "Switzerland",
        "AT" => "Austria",
        "PL" => "Poland",
        "CZ" => "Czechia",
        "GR" => "Greece",
        "HR" => "Croatia",
        "FI" => "Finland",
        "IS" => "Iceland",
        "US" => "United States USA",
        "CA" => "Canada",
        "JP" => "Japan",
        "CN" => "China",
        "KR" => "South Korea",
        "TH" => "Thailand",
        "AU" => "Australia",
        "MX" => "Mexico",
        "BR" => "Brazil",
        "TR" => "Turkey",
        "EG" => "Egypt",
        "MA" => "Morocco",
        "AE" => "United Arab Emirates",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copenhagen_coordinates_name_a_capital_region_place() {
        let p = place_for(55.6761, 12.5683).unwrap();
        assert_eq!(p.country.as_deref(), Some("DK"));
        assert!(haversine_km(55.6761, 12.5683, 55.6761, 12.5683) < 1e-9);
        let d = haversine_km(55.6761, 12.5683, 41.9028, 12.4964);
        assert!((d - 1531.0).abs() < 15.0, "{d}");
        assert!(place_for(0.0, 0.0).is_none());
    }
}
