//! AU Find building directory. Only building number, status and latency enter logs.
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::{gazetteer::AuRoom, geocode::GeoResult, kernel};

#[derive(Deserialize)]
struct Building {
    id: String,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    address: String,
    latitude: f64,
    longitude: f64,
}

pub async fn lookup(http: &reqwest::Client, base: &str, room: &AuRoom) -> Option<GeoResult> {
    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_millis(1500), async {
        let response = http
            .get(format!("{base}/api/buildings"))
            .query(&[("q", &room.building)])
            .send()
            .await
            .map_err(|_| "transport_error")?;
        if !response.status().is_success() {
            return Err("http_error");
        }
        let buildings: Vec<Building> = response.json().await.map_err(|_| "invalid_response")?;
        let Some(building) = buildings.into_iter().find(|b| b.id == room.building) else {
            return Err("no_exact_match");
        };
        let (lat, lng) = kernel::parse_coord(building.latitude, building.longitude)
            .ok_or("invalid_coordinates")?;
        let name = building
            .names
            .iter()
            .map(|n| kernel::one_line(n))
            .find(|n| !n.is_empty() && !n.to_lowercase().starts_with("bygning "));
        let title = match &name {
            Some(name) => format!("Aarhus Universitet {} {name}", room.building),
            None => format!("Aarhus Universitet {}", room.building),
        };
        let address = kernel::one_line(&building.address)
            .split(',')
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(", ");
        Ok(GeoResult {
            name: title,
            address,
            label: Some(room.label(name.as_deref())),
            source: Some("AU Find".into()),
            lat: kernel::e7_to_deg(lat),
            lng: kernel::e7_to_deg(lng),
        })
    })
    .await
    .unwrap_or(Err("timeout"));
    let status = match &result {
        Ok(_) => "hit",
        Err(status) => status,
    };
    tracing::info!(building = %room.building, status, latency_ms = start.elapsed().as_millis() as u64, "aufind");
    result.ok()
}
