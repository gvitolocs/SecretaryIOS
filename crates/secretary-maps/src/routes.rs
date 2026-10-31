//! Concurrent per-mode Routes calls: Google when a key is loaded, else (or when every Google
//! mode fails) the self-hosted OSRM, else with `estimate=1` straight-line estimates. Only
//! mode/status/latency enter tracing.
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

const MASK: &str = "routes.duration,routes.staticDuration,routes.distanceMeters,routes.polyline.encodedPolyline,routes.legs.steps.transitDetails,routes.legs.steps.navigationInstruction,routes.legs.steps.distanceMeters,routes.legs.steps.staticDuration,routes.legs.steps.polyline.encodedPolyline,routes.legs.steps.travelMode";
const MODES: [&str; 4] = ["drive", "walk", "bike", "transit"];
#[derive(Deserialize)]
pub struct RoutesQuery {
    from: Option<String>,
    to: Option<String>,
    modes: Option<String>,
    arrive: Option<String>,
    /// `1`: answer straight-line estimates (`"provider":"estimate"`) when no router answers,
    /// instead of 503. Lucina asks for them; the apps make their own "≈" estimates.
    estimate: Option<String>,
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
/// A decoded precision-5 polyline as `[[lat, lng], ...]`, at most `max` points.
pub(crate) fn polyline_json(points: &[(i64, i64)], max: usize) -> Vec<[f64; 2]> {
    kernel::simplify(points, max)
        .iter()
        .map(|(a, b)| [*a as f64 / 1e5, *b as f64 / 1e5])
        .collect()
}
/// One turn-by-turn step: `None` when it has neither an instruction nor a maneuver.
pub(crate) fn step(
    instruction: &str,
    maneuver: Option<&str>,
    distance_m: u64,
    duration_s: u64,
    polyline: Option<&str>,
) -> Option<Value> {
    let instruction = kernel::one_line(instruction);
    let maneuver = maneuver.filter(|m| !m.is_empty() && *m != "MANEUVER_UNSPECIFIED");
    if instruction.is_empty() && maneuver.is_none() {
        return None;
    }
    // A step with a malformed polyline keeps its instruction.
    let points = polyline
        .and_then(kernel::decode_polyline)
        .map(|p| polyline_json(&p, STEP_POINTS))
        .unwrap_or_default();
    Some(
        json!({"instruction":instruction,"maneuver":maneuver,"distanceM":distance_m,"durationS":duration_s,"polyline":points}),
    )
}
pub(crate) const MAX_STEPS: usize = 200;
const STEP_POINTS: usize = 100;
/// Google's steps over all legs (every mode), at most [`MAX_STEPS`].
fn google_steps(r: &Value) -> Vec<Value> {
    r["legs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|leg| leg["steps"].as_array())
        .flatten()
        .filter_map(|s| {
            let nav = &s["navigationInstruction"];
            step(
                nav["instructions"].as_str().unwrap_or(""),
                nav["maneuver"].as_str(),
                // Google omits zero values.
                s["distanceMeters"].as_u64().unwrap_or(0),
                seconds(&s["staticDuration"]).unwrap_or(0),
                s["polyline"]["encodedPolyline"].as_str(),
            )
        })
        .take(MAX_STEPS)
        .collect()
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
    let points = polyline_json(&points, 400);
    let mut out = json!({"mode":mode,"durationS":duration,"staticDurationS":static_duration,"distanceM":distance,"polyline":points,"steps":google_steps(r)});
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
        let mut body = json!({"origin":waypoint(from),"destination":waypoint(to),"travelMode":match mode {"drive"=>"DRIVE","walk"=>"WALK","bike"=>"BICYCLE",_=>"TRANSIT"},"polylineQuality":"OVERVIEW","polylineEncoding":"ENCODED_POLYLINE","languageCode":"da"});
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
    let (mut provider, mut routes, mut failed) = ("google", Vec::new(), Vec::new());
    if st.config.google_key.is_some() {
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
        for (mode, task) in tasks {
            match task.await {
                Ok(Ok(v)) => routes.push(v),
                _ => failed.push(mode),
            }
        }
    }
    if routes.is_empty() {
        if let Some(base) = st.config.osrm_url.clone() {
            (provider, failed) = ("osrm", Vec::new());
            let mut tasks = Vec::new();
            for mode in MODES {
                if requested.contains(&mode) {
                    let (st, base) = (st.clone(), base.clone());
                    tasks.push((
                        mode,
                        tokio::spawn(
                            async move { osrm_one(st, &base, from, to, mode, arrive).await },
                        ),
                    ));
                }
            }
            for (mode, task) in tasks {
                match task.await {
                    Ok(Ok(v)) => routes.push(v),
                    _ => failed.push(mode),
                }
            }
        }
    }
    if routes.is_empty() && p.estimate.as_deref() == Some("1") {
        (provider, failed) = ("estimate", Vec::new());
        for mode in MODES {
            if requested.contains(&mode) {
                match crate::osrm::estimate(from, to, mode) {
                    Some(mut v) => {
                        if let Some(a) = arrive {
                            let leave = timestamp(kernel::leave_at(
                                a,
                                v["durationS"].as_u64().unwrap_or(0),
                                300,
                            ));
                            v["leaveAt"] = json!(leave);
                        }
                        routes.push(v)
                    }
                    None => failed.push(mode),
                }
            }
        }
    }
    if routes.is_empty() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "provider_unavailable");
    }
    Json(json!({"provider":provider,"routes":routes,"failed":failed})).into_response()
}

/// One OSRM mode, cached like Google's (under its own key).
async fn osrm_one(
    st: Arc<AppState>,
    base: &str,
    from: (i64, i64),
    to: (i64, i64),
    mode: &'static str,
    arrive: Option<i64>,
) -> Result<Value, Unavailable> {
    let key = format!("osrm|{}", kernel::route_cache_key(from, to, mode, None));
    let cached = st.cache.lock().unwrap().get(&key);
    let result = match cached {
        Some(v) => Ok(v),
        None => {
            let start = std::time::Instant::now();
            let result = crate::osrm::route(&st.http, base, from, to, mode).await;
            tracing::info!(
                mode,
                status = if result.is_ok() { "ok" } else { "failed" },
                latency_ms = start.elapsed().as_millis() as u64,
                "routes osrm"
            );
            if let Ok(v) = &result {
                st.cache
                    .lock()
                    .unwrap()
                    .insert(key, v.clone(), 86400, false);
            }
            result
        }
    };
    let mut v = result?;
    if let Some(a) = arrive {
        let d = v["durationS"].as_u64().ok_or(Unavailable)?;
        v["leaveAt"] = json!(timestamp(kernel::leave_at(a, d, 300)).ok_or(Unavailable)?);
    }
    Ok(v)
}
