//! AU Find (Aarhus University) for secretary-server and the Lucina MCP:
//! `/api/secretary/au/*`, on top of `lucina_tools::tools::au` (af-server on
//! nezopt, `LUCINA_AU_URL`).
//!
//! - `GET /au/health`                 `{reachable, af_server, error?}` (always 200)
//! - `GET /au/people?q=&lang=da|en`   the owner's people procedure (retries, top 3)
//! - `GET /au/buildings?q=`           building lookup
//! - `GET /au/map.png?building=&z=`   a building outlined, or `?lat=&lng=&z=` a point
//!
//! Read-only. 404 `au_disabled` when `LUCINA_AU=off`; 503 `au_unavailable`
//! when af-server is down.

use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use lucina_tools::tools::au::{self, AuClient, AuError, MapTarget, Status};
use serde_json::{json, Value};

use crate::api::{ok, query_param, ApiError};
use crate::AppState;

const MAX_QUERY: usize = 200;

fn text_param(uri: &Uri, key: &str) -> Result<String, ApiError> {
    let v = query_param(uri, key).unwrap_or_default();
    let v = v.trim();
    if v.is_empty() || v.chars().count() > MAX_QUERY {
        return Err(ApiError::bad_request(format!(
            "{key} must be 1-{MAX_QUERY} characters"
        )));
    }
    Ok(v.to_owned())
}

fn lang_param(uri: &Uri) -> Result<&'static str, ApiError> {
    match query_param(uri, "lang").as_deref() {
        None | Some("") | Some("en") => Ok("en"),
        Some("da") => Ok("da"),
        Some(_) => Err(ApiError::bad_request("lang must be da or en")),
    }
}

fn number(uri: &Uri, key: &str) -> Result<Option<f64>, ApiError> {
    match query_param(uri, key) {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(Some)
            .ok_or_else(|| ApiError::bad_request(format!("{key} must be a number"))),
    }
}

/// The map a query asks for: `building` (+ `z`, default 17) or `lat`/`lng` (+ `z`, default 15).
pub fn map_target(uri: &Uri) -> Result<MapTarget, ApiError> {
    let z = match query_param(uri, "z") {
        None => None,
        Some(z) => {
            let z: u8 = z
                .trim()
                .parse()
                .map_err(|_| ApiError::bad_request("z must be 14-18"))?;
            if !(au::MIN_ZOOM..=au::MAX_ZOOM).contains(&z) {
                return Err(ApiError::bad_request("z must be 14-18"));
            }
            Some(z)
        }
    };
    if let Some(id) = query_param(uri, "building") {
        let id = id.trim().to_uppercase();
        if !au::valid_building_id(&id) {
            return Err(ApiError::bad_request(
                "building must be 1-6 letters or digits",
            ));
        }
        return Ok(MapTarget::Building {
            id,
            z: z.unwrap_or(au::BUILDING_ZOOM),
        });
    }
    match (number(uri, "lat")?, number(uri, "lng")?) {
        (Some(lat), Some(lng)) if au::in_denmark(lat, lng) => Ok(MapTarget::Point {
            lat,
            lng,
            z: z.unwrap_or(au::CAMPUS_ZOOM),
        }),
        (Some(_), Some(_)) => Err(ApiError::bad_request("lat/lng must be inside Denmark")),
        (None, None) => Ok(MapTarget::Point {
            lat: au::PARK.0,
            lng: au::PARK.1,
            z: z.unwrap_or(au::CAMPUS_ZOOM),
        }),
        _ => Err(ApiError::bad_request("give both lat and lng, or building")),
    }
}

fn unavailable(e: impl Into<String>) -> ApiError {
    ApiError::new(503, "au_unavailable", e)
}

fn status_code(status: Status, error: Option<&str>) -> Result<(), ApiError> {
    match status {
        Status::Unavailable => Err(unavailable(
            error.unwrap_or("AU Find (af-server) is unreachable"),
        )),
        _ => Ok(()),
    }
}

pub async fn route(
    st: &AppState,
    method: &Method,
    uri: &Uri,
    parts: &[String],
) -> Result<Response, ApiError> {
    if !st.cfg.au_enabled {
        return Err(ApiError::new(
            404,
            "au_disabled",
            "AU Find is off (LUCINA_AU)",
        ));
    }
    if *method != Method::GET {
        return Err(ApiError::method_not_allowed(&["GET"]));
    }
    let client = AuClient::new(&st.tool_env.http, &st.cfg.au_url);
    match parts.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["health"] => Ok(ok(match client.health_fresh().await {
            Ok(h) => json!({"reachable": true, "af_server": h}),
            Err(e) => json!({"reachable": false, "af_server": Value::Null, "error": e}),
        })),
        ["people"] => {
            let q = text_param(uri, "q")?;
            let lang = lang_param(uri)?;
            let r = client.people_search(&q, lang).await;
            status_code(r.status, r.error.as_deref())?;
            Ok(ok(serde_json::to_value(&r).unwrap_or(Value::Null)))
        }
        ["buildings"] => {
            let q = text_param(uri, "q")?;
            let r = client.building(&q).await;
            status_code(r.status, r.error.as_deref())?;
            Ok(ok(serde_json::to_value(&r).unwrap_or(Value::Null)))
        }
        ["map.png"] => {
            let target = map_target(uri)?;
            let png = client.map_png(&target).await.map_err(|e| match e {
                AuError::NotFound => ApiError::new(404, "not_found", "No such AU building"),
                AuError::BadRequest(m) => ApiError::bad_request(m),
                AuError::Unavailable(m) => unavailable(m),
            })?;
            let mut resp = (StatusCode::OK, png).into_response();
            resp.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
            resp.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static("private, max-age=3600"),
            );
            Ok(resp)
        }
        _ => Err(ApiError::not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(q: &str) -> Result<MapTarget, u16> {
        let uri: Uri = format!("/api/secretary/au/map.png?{q}").parse().unwrap();
        map_target(&uri).map_err(|e| e.status.as_u16())
    }

    #[test]
    fn map_targets() {
        assert_eq!(
            target("building=1532"),
            Ok(MapTarget::Building {
                id: "1532".into(),
                z: 17
            })
        );
        assert_eq!(
            target("building=113a&z=18"),
            Ok(MapTarget::Building {
                id: "113A".into(),
                z: 18
            })
        );
        assert_eq!(
            target(""),
            Ok(MapTarget::Point {
                lat: 56.1697,
                lng: 10.2039,
                z: 15
            })
        );
        assert_eq!(
            target("lat=56.17&lng=10.2&z=16"),
            Ok(MapTarget::Point {
                lat: 56.17,
                lng: 10.2,
                z: 16
            })
        );
        assert_eq!(target("building=../x"), Err(400));
        assert_eq!(target("building=1532&z=19"), Err(400));
        assert_eq!(target("building=1532&z=abc"), Err(400));
        assert_eq!(target("lat=48.8&lng=2.3"), Err(400));
        assert_eq!(target("lat=56.17"), Err(400));
        assert_eq!(target("lat=NaN&lng=10.2"), Err(400));
    }

    #[test]
    fn params() {
        let uri: Uri = "/x?q=%20%20&lang=it".parse().unwrap();
        assert_eq!(text_param(&uri, "q").unwrap_err().status.as_u16(), 400);
        assert_eq!(lang_param(&uri).unwrap_err().status.as_u16(), 400);
        let uri: Uri = "/x?q=Vittrup&lang=da".parse().unwrap();
        assert_eq!(text_param(&uri, "q").unwrap(), "Vittrup");
        assert_eq!(lang_param(&uri).unwrap(), "da");
    }
}
