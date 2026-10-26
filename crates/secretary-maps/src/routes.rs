//! Concurrent per-mode Routes calls. Only mode/status/latency enter tracing.
use crate::provider::Unavailable;
use crate::{error, kernel, provider, AppState};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

const MASK: &str = "routes.duration,routes.staticDuration,routes.distanceMeters,routes.polyline.encodedPolyline,routes.legs.steps.transitDetails";
const MODES: [&str; 4] = ["drive", "walk", "bike", "transit"];
#[derive(Deserialize)]
pub struct RoutesQuery {
    from: Option<String>,
    to: Option<String>,
    modes: Option<String>,
    arrive: Option<String>,
}
fn coord(s: Option<&str>) -> Option<(i64, i64)> {
    let (a, b) = s?.split_once(',')?;
    kernel::parse_coord(a.trim().parse().ok()?, b.trim().parse().ok()?)
}
fn timestamp(ms: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000)
        .ok()?
        .format(&Rfc3339)
        .ok()
}
fn seconds(v: &Value) -> Option<u64> {
    let s = v.as_str()?.strip_suffix('s')?;
    let n: f64 = s.parse().ok()?;
    (n.is_finite() && n >= 0.0 && n < u64::MAX as f64).then(|| n.ceil() as u64)
}
fn waypoint(p: (i64, i64)) -> Value {
    json!({"location":{"latLng":{"latitude":kernel::e7_to_deg(p.0),"longitude":kernel::e7_to_deg(p.1)}}})
}
fn parse_route(body: &Value, mode: &str, arrive: Option<i64>) -> Option<Value> {
    let r = body.get("routes")?.as_array()?.first()?;
    let duration = seconds(&r["duration"])?;
    let static_duration = seconds(&r["staticDuration"])?;
    let distance = r["distanceMeters"].as_u64()?;
    let points = kernel::decode_polyline(r["polyline"]["encodedPolyline"].as_str()?)?;
    if points.is_empty() {
        return None;
    }
    let points: Vec<_> = kernel::simplify(&points, 400)
        .iter()
        .map(|(a, b)| [*a as f64 / 1e5, *b as f64 / 1e5])
        .collect();
    let mut out = json!({"mode":mode,"durationS":duration,"staticDurationS":static_duration,"distanceM":distance,"polyline":points});
    if let Some(arrive) = arrive {
        out["leaveAt"] = json!(timestamp(kernel::leave_at(arrive, duration, 300))?);
    }
    if mode == "transit" {
        let mut transit = Vec::new();
        if let Some(legs) = r["legs"].as_array() {
            for leg in legs {
                if let Some(steps) = leg["steps"].as_array() {
                    for step in steps {
                        if let Some(t) = step.get("transitDetails") {
                            let line = t["transitLine"]["nameShort"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .or(t["transitLine"]["name"].as_str())
                                .unwrap_or("");
                            let stops = &t["stopDetails"];
                            transit.push(json!({"line":line,"vehicle":t["transitLine"]["vehicle"]["type"],"from":stops["departureStop"]["name"],"to":stops["arrivalStop"]["name"],"departAt":stops["departureTime"],"arriveAt":stops["arrivalTime"]}));
                        }
                    }
                }
            }
        }
        out["transit"] = json!(transit);
    }
    Some(out)
}
async fn one(
    st: Arc<AppState>,
    from: (i64, i64),
    to: (i64, i64),
    mode: &'static str,
    arrive: Option<i64>,
) -> Result<Value, Unavailable> {
    let key = kernel::route_cache_key(from, to, mode, arrive);
    if let Some(mut v) = st.cache.lock().unwrap().get(&key) {
        // Arrival buckets can differ by seconds: recompute leaveAt for this exact request.
        if let Some(a) = arrive {
            v["leaveAt"] = json!(timestamp(kernel::leave_at(
                a,
                v["durationS"].as_u64().ok_or(Unavailable)?,
                300
            ))
            .ok_or(Unavailable)?);
        }
        return Ok(v);
    }
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_secs(4), async {
        let mut body = json!({"origin":waypoint(from),"destination":waypoint(to),"travelMode":match mode {"drive"=>"DRIVE","walk"=>"WALK","bike"=>"BICYCLE",_=>"TRANSIT"},"polylineQuality":"OVERVIEW","polylineEncoding":"ENCODED_POLYLINE"});
        let now = OffsetDateTime::now_utc();
        if mode == "drive" || mode == "transit" {
            body["departureTime"] = json!(now.format(&Rfc3339).map_err(|_| Unavailable)?);
        }
        if mode == "transit" {
            if let Some(a) = arrive { body.as_object_mut().unwrap().remove("departureTime"); body["arrivalTime"] = json!(timestamp(a).ok_or(Unavailable)?); }
        }
        if mode == "drive" { body["routingPreference"] = json!("TRAFFIC_AWARE"); }
        let mut response = provider::post(&st,&st.config.routes_url,MASK,&body).await?;
        // Google drive accepts departureTime, not arrivalTime. First obtain static duration,
        // then route at the inferred departure for a future appointment (same 4 s deadline).
        if mode == "drive" {
            if let Some(a) = arrive.filter(|a| *a as i128 > now.unix_timestamp_nanos()/1_000_000) {
                let d = seconds(&response["routes"][0]["staticDuration"]).ok_or(Unavailable)?;
                let departure = kernel::leave_at(a,d,0);
                body["departureTime"] = json!(timestamp(departure).ok_or(Unavailable)?);
                response = provider::post(&st,&st.config.routes_url,MASK,&body).await?;
            }
        }
        parse_route(&response,mode,arrive).ok_or(Unavailable)
    }).await.map_err(|_| Unavailable).and_then(|r| r);
    tracing::info!(
        mode,
        status = if result.is_ok() { "ok" } else { "failed" },
        latency_ms = start.elapsed().as_millis() as u64,
        "routes"
    );
    if let Ok(v) = &result {
        st.cache.lock().unwrap().insert(
            key,
            v.clone(),
            if matches!(mode, "drive" | "transit") {
                300
            } else {
                86400
            },
            false,
        );
    }
    result
}

pub async fn routes_route(
    State(st): State<Arc<AppState>>,
    Query(p): Query<RoutesQuery>,
) -> Response {
    let (Some(from), Some(to)) = (coord(p.from.as_deref()), coord(p.to.as_deref())) else {
        return error(StatusCode::BAD_REQUEST, "bad_coordinates");
    };
    let requested: Vec<_> = p
        .modes
        .as_deref()
        .map(|s| s.split(',').map(str::trim).collect())
        .unwrap_or_else(|| MODES.to_vec());
    if requested.is_empty() || requested.iter().any(|m| !MODES.contains(m)) {
        return error(StatusCode::BAD_REQUEST, "bad_modes");
    }
    let arrive = match p.arrive {
        None => None,
        Some(s) => match OffsetDateTime::parse(&s, &Rfc3339)
            .ok()
            .and_then(|t| i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok())
        {
            Some(ms) => Some(ms),
            None => return error(StatusCode::BAD_REQUEST, "bad_arrive"),
        },
    };
    if st.config.google_key.is_none() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "provider_unavailable");
    }
    let mut tasks = Vec::new();
    for mode in MODES {
        if requested.contains(&mode) {
            let st = st.clone();
            tasks.push((
                mode,
                tokio::spawn(async move { one(st, from, to, mode, arrive).await }),
            ));
        }
    }
    let (mut routes, mut failed) = (Vec::new(), Vec::new());
    for (mode, task) in tasks {
        match task.await {
            Ok(Ok(v)) => routes.push(v),
            _ => failed.push(mode),
        }
    }
    if routes.is_empty() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "provider_unavailable");
    }
    Json(json!({"provider":"google","routes":routes,"failed":failed})).into_response()
}
