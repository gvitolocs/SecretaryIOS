//! Self-hosted OSRM (`MAPS_OSRM_URL`, `GET /route/v1/<profile>/<lng>,<lat>;<lng>,<lat>`): drive,
//! walk and bike routes when there is no Google key or Google fails. OSRM has no transit and no
//! traffic, so `staticDurationS` equals `durationS`. Steps (`steps=true`) carry the road name as
//! the instruction and OSRM's maneuver mapped to Google's names. Only mode/status/latency enter
//! tracing.
use crate::provider::Unavailable;
use crate::{kernel, routes};
use serde_json::{json, Value};
use std::time::Duration;

/// The OSRM profile of a route mode (`None`: transit, which OSRM cannot route).
pub fn profile(mode: &str) -> Option<&'static str> {
    match mode {
        "drive" => Some("driving"),
        "walk" => Some("walking"),
        "bike" => Some("cycling"),
        _ => None,
    }
}

/// OSRM's maneuver (`type`, `modifier`) as Google's `Maneuver` name, `None` without one.
fn maneuver(kind: &str, modifier: &str) -> Option<&'static str> {
    let side = |left, right| match modifier {
        m if m.contains("left") => Some(left),
        m if m.contains("right") => Some(right),
        _ => None,
    };
    match kind {
        "depart" => Some("DEPART"),
        "arrive" => None,
        "new name" => Some("NAME_CHANGE"),
        "merge" => Some("MERGE"),
        "on ramp" | "off ramp" => side("RAMP_LEFT", "RAMP_RIGHT"),
        "fork" => side("FORK_LEFT", "FORK_RIGHT"),
        "roundabout" | "rotary" | "roundabout turn" => side("ROUNDABOUT_LEFT", "ROUNDABOUT_RIGHT"),
        _ => match modifier {
            "uturn" => Some("UTURN_LEFT"),
            "sharp left" => Some("TURN_SHARP_LEFT"),
            "left" => Some("TURN_LEFT"),
            "slight left" => Some("TURN_SLIGHT_LEFT"),
            "straight" => Some("STRAIGHT"),
            "slight right" => Some("TURN_SLIGHT_RIGHT"),
            "right" => Some("TURN_RIGHT"),
            "sharp right" => Some("TURN_SHARP_RIGHT"),
            _ => None,
        },
    }
}

/// OSRM's answer to the route shape of `/v1/maps/routes` (without `leaveAt`).
pub fn parse_route(body: &Value, mode: &str) -> Option<Value> {
    if body.get("code")?.as_str()? != "Ok" {
        return None;
    }
    let r = body.get("routes")?.as_array()?.first()?;
    let finite = |v: &Value| {
        v.as_f64()
            .filter(|x| x.is_finite() && *x >= 0.0 && *x < 1e9)
    };
    let duration = finite(&r["duration"])?.ceil() as u64;
    let distance = finite(&r["distance"])?.round() as u64;
    let points = kernel::decode_polyline(r["geometry"].as_str()?)?;
    let points = routes::polyline_json(&points, 400);
    let steps: Vec<_> = r["legs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|leg| leg["steps"].as_array())
        .flatten()
        .filter_map(|s| {
            let m = &s["maneuver"];
            routes::step(
                s["name"].as_str().unwrap_or(""),
                maneuver(
                    m["type"].as_str().unwrap_or(""),
                    m["modifier"].as_str().unwrap_or(""),
                ),
                finite(&s["distance"]).map_or(0, |d| d.round() as u64),
                finite(&s["duration"]).map_or(0, |d| d.ceil() as u64),
                s["geometry"].as_str(),
            )
        })
        .take(routes::MAX_STEPS)
        .collect();
    Some(
        json!({"mode":mode,"durationS":duration,"staticDurationS":duration,"distanceM":distance,"polyline":points,"steps":steps}),
    )
}

/// One OSRM route within four seconds.
pub async fn route(
    http: &reqwest::Client,
    base: &str,
    from: (i64, i64),
    to: (i64, i64),
    mode: &str,
) -> Result<Value, Unavailable> {
    let profile = profile(mode).ok_or(Unavailable)?;
    let deg = kernel::e7_to_deg;
    let url = format!(
        "{base}/route/v1/{profile}/{},{};{},{}",
        deg(from.1),
        deg(from.0),
        deg(to.1),
        deg(to.0)
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        let res = http
            .get(url)
            .query(&[
                ("overview", "simplified"),
                ("geometries", "polyline"),
                ("steps", "true"),
            ])
            .send()
            .await
            .map_err(|_| Unavailable)?;
        if !res.status().is_success() {
            return Err(Unavailable);
        }
        let body: Value = res.json().await.map_err(|_| Unavailable)?;
        parse_route(&body, mode).ok_or(Unavailable)
    })
    .await
    .map_err(|_| Unavailable)?
}

/// The straight-line estimate of a mode (`estimate=1` when no router answered): the verified
/// MK4 travel time over the haversine distance, no polyline. `None` for transit.
pub fn estimate(from: (i64, i64), to: (i64, i64), mode: &str) -> Option<Value> {
    let m = match mode {
        "walk" => kernel::Mode::Walk,
        "bike" => kernel::Mode::Bike,
        "drive" => kernel::Mode::Car,
        _ => return None,
    };
    let d = kernel::distance_m(from, to);
    let duration = u64::from(kernel::travel_estimate_min(d, m)) * 60;
    Some(
        json!({"mode":mode,"durationS":duration,"staticDurationS":duration,"distanceM":d.round() as u64,"polyline":[],"steps":[],"estimate":true}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osrm_answers() {
        let ok = json!({"code":"Ok","routes":[{"duration":539.6,"distance":4200.4,"geometry":"_p~iF~ps|U_ulLnnqC_mqNvxq`@"}]});
        let r = parse_route(&ok, "bike").unwrap();
        assert_eq!(r["durationS"], 540);
        assert_eq!(r["staticDurationS"], 540);
        assert_eq!(r["distanceM"], 4200);
        assert_eq!(r["polyline"][0], json!([38.5, -120.2]));
        assert_eq!(r["steps"], json!([]));
        let with_steps = json!({"code":"Ok","routes":[{"duration":60,"distance":300,"geometry":"_p~iF~ps|U_ulLnnqC_mqNvxq`@","legs":[{"steps":[
            {"name":"Møllestien","distance":120.4,"duration":30.2,"geometry":"_p~iF~ps|U_ulLnnqC","maneuver":{"type":"depart"}},
            {"name":"","distance":180,"duration":30,"geometry":"_ulLnnqC_mqNvxq`@","maneuver":{"type":"turn","modifier":"slight left"}},
            {"name":"","distance":0,"duration":0,"geometry":"_mqNvxq`@","maneuver":{"type":"arrive"}}]}]}]});
        let steps = parse_route(&with_steps, "bike").unwrap()["steps"].clone();
        assert_eq!(
            steps,
            json!([{"instruction":"Møllestien","maneuver":"DEPART","distanceM":120,"durationS":31,"polyline":[[38.5,-120.2],[40.7,-120.95]]},
                {"instruction":"","maneuver":"TURN_SLIGHT_LEFT","distanceM":180,"durationS":30,"polyline":[[2.2,-0.75],[4.752,-6.253]]}])
        );
        for bad in [
            json!({"code":"NoRoute","routes":[]}),
            json!({"code":"Ok","routes":[]}),
            json!({"code":"Ok","routes":[{"duration":-1,"distance":1,"geometry":"??"}]}),
            json!({"code":"Ok","routes":[{"duration":1,"distance":1,"geometry":"_"}]}),
        ] {
            assert!(parse_route(&bad, "walk").is_none(), "{bad}");
        }
        assert_eq!(profile("transit"), None);
    }

    #[test]
    fn straight_line_estimates() {
        // Skejby to Folkehuset: about 3.95 km in a straight line.
        let (home, folkehuset) = ((561_963_300, 102_011_000), (561_611_000, 102_006_000));
        let walk = estimate(home, folkehuset, "walk").unwrap();
        let d = kernel::distance_m(home, folkehuset);
        assert_eq!(walk["distanceM"], d.round() as u64);
        assert_eq!(
            walk["durationS"],
            u64::from(kernel::travel_estimate_min(d, kernel::Mode::Walk)) * 60
        );
        assert_eq!(walk["estimate"], true);
        assert!(
            estimate(home, folkehuset, "drive").unwrap()["durationS"].as_u64()
                < walk["durationS"].as_u64()
        );
        assert!(estimate(home, folkehuset, "transit").is_none());
    }
}
