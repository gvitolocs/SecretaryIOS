//! Persistent calendar history, independent of a phone's rolling report.
use crate::{
    events::CalendarEvent,
    store::{read_json, write_json},
    App,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub event: CalendarEvent,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct Data {
    generation: u64,
    events: BTreeMap<String, Entry>,
}
pub struct CalendarCache {
    path: PathBuf,
    data: Mutex<Data>,
}
impl CalendarCache {
    pub fn open(root: &Path) -> std::io::Result<Self> {
        let path = root.join("calendar-cache.json");
        let mut data: Data = read_json(&path)?.unwrap_or_default();
        let len = data.events.len();
        let cutoff = (Utc::now() - Duration::days(180)).timestamp_millis();
        data.events.retain(|_, e| e.event.end_ms >= cutoff);
        if data.events.len() != len {
            write_json(&path, &data)?;
        }
        Ok(Self {
            data: Mutex::new(data),
            path,
        })
    }
    pub fn generation(&self) -> u64 {
        self.data.lock().unwrap().generation
    }
    pub fn merge(&self, events: Vec<CalendarEvent>, now: DateTime<Utc>) -> std::io::Result<()> {
        let mut guard = self.data.lock().unwrap();
        let mut data = guard.clone();
        let cutoff = (now - Duration::days(180)).timestamp_millis();
        for event in events.into_iter().filter(|e| e.end_ms >= cutoff) {
            if data.events.get(&event.id).is_none_or(|e| e.event != event) {
                data.generation += 1;
            }
            let first_seen = data.events.get(&event.id).map_or(now, |e| e.first_seen);
            data.events.insert(
                event.id.clone(),
                Entry {
                    event,
                    first_seen,
                    last_seen: now,
                },
            );
        }
        data.events.retain(|_, e| e.event.end_ms >= cutoff);
        write_json(&self.path, &data)?;
        *guard = data;
        Ok(())
    }
    pub fn window(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> (u64, Vec<CalendarEvent>) {
        let data = self.data.lock().unwrap();
        (
            data.generation,
            data.events
                .values()
                .filter(|e| {
                    e.event.end_ms > from.timestamp_millis()
                        && e.event.start_ms < to.timestamp_millis()
                        && e.event.end_ms >= (Utc::now() - Duration::days(180)).timestamp_millis()
                })
                .map(|e| e.event.clone())
                .collect(),
        )
    }
}
pub async fn refresh(app: &App) -> Result<(), String> {
    let now = Utc::now();
    let events =
        crate::events::fetch(app, now - Duration::days(14), now + Duration::days(30)).await?;
    app.calendar.merge(events, now).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upsert_change_expiry_and_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CalendarCache::open(dir.path()).unwrap();
        let now = Utc::now();
        let mut event: CalendarEvent = serde_json::from_value(serde_json::json!({
            "id":"iphone:a", "source":"iphone", "sourceId":"a", "title":"Lecture",
            "start":"now", "end":"later", "startMs":now.timestamp_millis(),
            "endMs":(now + Duration::hours(1)).timestamp_millis(), "allDay":false, "recurring":false
        }))
        .unwrap();
        cache.merge(vec![event.clone()], now).unwrap();
        assert_eq!(cache.generation(), 1);
        cache
            .merge(vec![event.clone()], now + Duration::hours(1))
            .unwrap();
        assert_eq!(cache.generation(), 1);
        event.title = "Changed lecture".into();
        cache
            .merge(vec![event.clone()], now + Duration::hours(2))
            .unwrap();
        assert_eq!(cache.generation(), 2);
        let reopened = CalendarCache::open(dir.path()).unwrap();
        assert_eq!(reopened.generation(), 2);
        assert_eq!(
            reopened
                .window(now - Duration::hours(1), now + Duration::days(1))
                .1,
            vec![event]
        );
        let data = reopened.data.lock().unwrap();
        assert_eq!(data.events["iphone:a"].first_seen, now);
        assert_eq!(data.events["iphone:a"].last_seen, now + Duration::hours(2));
        drop(data);
        reopened.merge(vec![], now + Duration::days(181)).unwrap();
        assert!(reopened.window(now, now + Duration::days(1)).1.is_empty());
        assert_eq!(reopened.generation(), 2);
    }
}
