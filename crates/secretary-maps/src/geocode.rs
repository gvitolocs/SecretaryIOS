//! Places API (New) Text Search. Queries and addresses never enter logs.
use crate::provider::Unavailable;
use crate::{kernel, provider, AppState};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GeoResult {
    pub name: String,
    pub address: String,
    pub lat: f64,
    pub lng: f64,
}

pub fn cache_key(q: &str, bias: Option<(i64, i64)>, limit: u8) -> String {
    let q = q
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let bias = bias.map(|(a, b)| {
        (
            (a + 50_000).div_euclid(100_000),
            (b + 50_000).div_euclid(100_000),
        )
    });
    format!("places|{limit}|{bias:?}|{q}")
}

pub async fn google(
    st: &AppState,
    q: &str,
    bias: Option<(i64, i64)>,
    limit: u8,
) -> Result<Vec<GeoResult>, Unavailable> {
    let mut body = json!({"textQuery":q,"languageCode":"da","pageSize":limit});
    if let Some((a, b)) = bias {
        body["locationBias"] = json!({"circle":{"center":{"latitude":kernel::e7_to_deg(a),"longitude":kernel::e7_to_deg(b)},"radius":50000.0}});
    }
    let response = provider::post(
        st,
        &st.config.places_url,
        "places.displayName,places.formattedAddress,places.location",
        &body,
    )
    .await?;
    if response.get("error").is_some() {
        return Err(Unavailable);
    }
    let places = match response.get("places") {
        None => return Ok(vec![]),
        Some(v) => v.as_array().ok_or(Unavailable)?,
    };
    Ok(places
        .iter()
        .filter_map(|p| {
            let loc = p.get("location")?;
            let (lat, lng) =
                kernel::parse_coord(loc["latitude"].as_f64()?, loc["longitude"].as_f64()?)?;
            let address = kernel::one_line(p["formattedAddress"].as_str().unwrap_or(""));
            let name = kernel::one_line(p["displayName"]["text"].as_str().unwrap_or(&address));
            if name.is_empty() {
                return None;
            }
            Some(GeoResult {
                name,
                address,
                lat: kernel::e7_to_deg(lat),
                lng: kernel::e7_to_deg(lng),
            })
        })
        .take(limit as usize)
        .collect())
}
