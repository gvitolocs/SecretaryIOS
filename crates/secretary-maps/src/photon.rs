//! Self-hosted Photon (`MAPS_PHOTON_URL`, `GET /api?q=&limit=&lat=&lon=&lang=`): the geocoder
//! when there is no Google key or Google fails. Queries and addresses never enter logs.
use crate::geocode::GeoResult;
use crate::kernel;
use crate::provider::Unavailable;
use serde_json::Value;
use std::time::Duration;

fn prop(p: &Value, key: &str) -> Option<String> {
    let s = kernel::one_line(p.get(key)?.as_str()?);
    (!s.is_empty()).then_some(s)
}

fn join_some(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    }
}

/// Photon's GeoJSON `FeatureCollection` to results: address = "street housenumber, postcode
/// city, country" without the missing parts; name falls back to the first address part.
/// Features with invalid coordinates or neither a name nor an address are dropped.
pub fn parse_features(body: &Value, limit: usize) -> Vec<GeoResult> {
    let Some(features) = body.get("features").and_then(Value::as_array) else {
        return Vec::new();
    };
    features
        .iter()
        .filter_map(|f| {
            let c = f.get("geometry")?.get("coordinates")?.as_array()?;
            let (lng, lat) = (c.first()?.as_f64()?, c.get(1)?.as_f64()?);
            let (lat_e7, lng_e7) = kernel::parse_coord(lat, lng)?;
            let p = f.get("properties").unwrap_or(&Value::Null);
            let street = prop(p, "street").map(|s| match prop(p, "housenumber") {
                Some(n) => format!("{s} {n}"),
                None => s,
            });
            let parts: Vec<String> = [
                street,
                join_some(prop(p, "postcode"), prop(p, "city")),
                prop(p, "country"),
            ]
            .into_iter()
            .flatten()
            .collect();
            let name = prop(p, "name").or_else(|| parts.first().cloned())?;
            Some(GeoResult {
                label: None,
                source: Some("Photon".into()),
                name: kernel::clip(&name, kernel::TITLE_MAX_CHARS),
                address: kernel::clip(&parts.join(", "), kernel::ADDRESS_MAX_CHARS),
                lat: kernel::e7_to_deg(lat_e7),
                lng: kernel::e7_to_deg(lng_e7),
            })
        })
        .take(limit)
        .collect()
}

/// One Photon query within three seconds. A transport error, non-2xx status or non-JSON body
/// is [`Unavailable`].
pub async fn geocode(
    http: &reqwest::Client,
    base: &str,
    q: &str,
    bias: Option<(i64, i64)>,
    limit: u8,
) -> Result<Vec<GeoResult>, Unavailable> {
    let mut params = vec![
        ("q", q.to_string()),
        ("limit", limit.to_string()),
        ("lang", "en".to_string()),
    ];
    if let Some((lat, lng)) = bias {
        params.push(("lat", kernel::e7_to_deg(lat).to_string()));
        params.push(("lon", kernel::e7_to_deg(lng).to_string()));
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        let res = http
            .get(format!("{base}/api"))
            .query(&params)
            .send()
            .await
            .map_err(|_| Unavailable)?;
        if !res.status().is_success() {
            return Err(Unavailable);
        }
        let body: Value = res.json().await.map_err(|_| Unavailable)?;
        Ok(parse_features(&body, limit as usize))
    })
    .await
    .map_err(|_| Unavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn features_to_results() {
        let body = json!({"features": [
            {"geometry": {"coordinates": [10.2006, 56.1611]},
             "properties": {"name": "Folkehuset", "street": "Møllestien", "housenumber": "7",
                            "postcode": "8000", "city": "Aarhus C", "country": "Denmark"}},
            {"geometry": {"coordinates": [10.2, 56.1]}, "properties": {"street": "Nørregade"}},
            {"geometry": {"coordinates": [0.0, 0.0]}, "properties": {"name": "Null Island"}},
            {"geometry": {"coordinates": [10.2, 56.1]}, "properties": {}},
            {"properties": {"name": "no geometry"}}
        ]});
        let r = parse_features(&body, 5);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].name, "Folkehuset");
        assert_eq!(r[0].address, "Møllestien 7, 8000 Aarhus C, Denmark");
        assert_eq!((r[0].lat, r[0].lng), (56.1611, 10.2006));
        assert_eq!(
            (r[1].name.as_str(), r[1].address.as_str()),
            ("Nørregade", "Nørregade")
        );
        assert_eq!(parse_features(&body, 1).len(), 1);
        assert!(parse_features(&json!({"type": "x"}), 5).is_empty());
    }
}
