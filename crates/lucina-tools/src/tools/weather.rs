//! `weather`: Open-Meteo forecast + geocoding (no key). Default location
//! from config (`LUCINA_DEFAULT_LOCATION_*`, Copenhagen, Denmark).
//!
//! With a commercial Open-Meteo plan, `secrets.toml` maps the setting
//! `weather.api_key` to a secret ref; the key is resolved here, right
//! before each request, and sent as the `apikey` parameter (point the
//! endpoints at `customer-api.open-meteo.com` then). It never appears in
//! the result.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{schema_for, Ctx, SideEffect, Tool, ToolError, ToolResult};

#[derive(Deserialize, JsonSchema)]
struct Args {
    /// Place name, optionally with country ("Copenhagen", "Roma, Italia"). Omit for the owner's default location.
    location: Option<String>,
    /// Forecast days including today, 1-7 (default 3).
    #[schemars(range(min = 1, max = 7))]
    days: Option<u8>,
}

/// WMO weather code to a short English description.
pub fn wmo(code: i64) -> &'static str {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "fog",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 => "light rain",
        63 => "rain",
        65 => "heavy rain",
        66 | 67 => "freezing rain",
        71 => "light snow",
        73 => "snow",
        75 => "heavy snow",
        77 => "snow grains",
        80 => "light showers",
        81 => "showers",
        82 => "violent showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "unknown",
    }
}

struct Place {
    name: String,
    country: Option<String>,
    admin: Option<String>,
    lat: f64,
    lon: f64,
}

async fn geocode(ctx: &Ctx, query: &str) -> Result<Place, ToolError> {
    let (name, country) = match query.split_once(',') {
        Some((n, c)) => (
            n.trim(),
            Some(c.trim().to_lowercase()).filter(|c| !c.is_empty()),
        ),
        None => (query.trim(), None),
    };
    let url = format!(
        "{}/v1/search",
        ctx.env.endpoints.open_meteo_geocoding.trim_end_matches('/')
    );
    let resp = ctx
        .env
        .http
        .get(url)
        .query(&[
            ("name", name),
            ("count", "10"),
            ("language", "en"),
            ("format", "json"),
        ])
        .send()
        .await
        .map_err(|e| ToolError::new(format!("geocoding unreachable: {}", e.without_url())))?;
    if !resp.status().is_success() {
        return Err(ToolError::new(format!(
            "geocoding HTTP {}",
            resp.status().as_u16()
        )));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| ToolError::new(format!("geocoding response: {e}")))?;
    let results = v["results"].as_array().cloned().unwrap_or_default();
    let matches_country = |r: &Value| {
        country.as_ref().is_none_or(|c| {
            r["country"].as_str().is_some_and(|x| {
                x.to_lowercase().contains(c.as_str()) || c.contains(&x.to_lowercase())
            }) || r["country_code"]
                .as_str()
                .is_some_and(|x| x.eq_ignore_ascii_case(c))
        })
    };
    let r = results
        .iter()
        .find(|r| matches_country(r))
        .or_else(|| results.first())
        .ok_or_else(|| ToolError::new(format!("location not found: {query}")))?;
    Ok(Place {
        name: r["name"].as_str().unwrap_or(name).to_owned(),
        country: r["country"].as_str().map(str::to_owned),
        admin: r["admin1"].as_str().map(str::to_owned),
        lat: r["latitude"]
            .as_f64()
            .ok_or_else(|| ToolError::new("geocoding: no latitude"))?,
        lon: r["longitude"]
            .as_f64()
            .ok_or_else(|| ToolError::new("geocoding: no longitude"))?,
    })
}

pub struct WeatherTool;

#[async_trait::async_trait]
impl Tool for WeatherTool {
    fn name(&self) -> &'static str {
        "weather"
    }
    fn description(&self) -> &'static str {
        "Current weather and a daily forecast (up to 7 days) for a place, from Open-Meteo. Without a location it uses the owner's default location."
    }
    fn parameters(&self) -> Value {
        schema_for::<Args>()
    }
    fn side_effect(&self) -> SideEffect {
        SideEffect::None
    }
    async fn call(&self, args: Value, ctx: &Ctx) -> ToolResult {
        let a: Args = super::args(args)?;
        let days = a.days.unwrap_or(3).clamp(1, 7);
        let place = match a
            .location
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(q) => geocode(ctx, q).await?,
            None => {
                let d = &ctx.env.default_location;
                Place {
                    name: d.name.clone(),
                    country: None,
                    admin: None,
                    lat: d.latitude,
                    lon: d.longitude,
                }
            }
        };
        let url = format!(
            "{}/v1/forecast",
            ctx.env.endpoints.open_meteo_forecast.trim_end_matches('/')
        );
        // Resolved right before the call (cached at most 10 minutes), never
        // returned: the value only goes into this request's query.
        let api_key = ctx.secret("weather.api_key", "weather").await?;
        let mut request = ctx
            .env
            .http
            .get(url)
            .query(&[
                ("latitude", place.lat.to_string()),
                ("longitude", place.lon.to_string()),
                ("current", "temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m".into()),
                ("daily", "weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max,sunrise,sunset".into()),
                ("timezone", "auto".into()),
                ("forecast_days", days.to_string()),
            ]);
        if let Some(key) = &api_key {
            request = request.query(&[("apikey", key.expose_secret())]);
        }
        let resp = request
            .send()
            .await
            .map_err(|e| ToolError::new(format!("forecast unreachable: {}", e.without_url())))?;
        if !resp.status().is_success() {
            return Err(ToolError::new(format!(
                "forecast HTTP {}",
                resp.status().as_u16()
            )));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::new(format!("forecast response: {e}")))?;
        let cur = &v["current"];
        let d = &v["daily"];
        let n = d["time"].as_array().map(Vec::len).unwrap_or(0);
        let daily: Vec<Value> = (0..n)
            .map(|i| {
                json!({
                    "date": d["time"][i],
                    "conditions": wmo(d["weather_code"][i].as_i64().unwrap_or(-1)),
                    "tempMaxC": d["temperature_2m_max"][i],
                    "tempMinC": d["temperature_2m_min"][i],
                    "precipitationMm": d["precipitation_sum"][i],
                    "precipitationProbability": d["precipitation_probability_max"][i],
                    "windMaxKmh": d["wind_speed_10m_max"][i],
                    "sunrise": d["sunrise"][i],
                    "sunset": d["sunset"][i],
                })
            })
            .collect();
        Ok(json!({
            "location": {"name": place.name, "region": place.admin, "country": place.country, "latitude": place.lat, "longitude": place.lon, "timezone": v["timezone"]},
            "current": {
                "time": cur["time"],
                "conditions": wmo(cur["weather_code"].as_i64().unwrap_or(-1)),
                "temperatureC": cur["temperature_2m"],
                "feelsLikeC": cur["apparent_temperature"],
                "humidity": cur["relative_humidity_2m"],
                "precipitationMm": cur["precipitation"],
                "windKmh": cur["wind_speed_10m"],
            },
            "daily": daily,
            "source": "Open-Meteo",
        }))
    }
}
