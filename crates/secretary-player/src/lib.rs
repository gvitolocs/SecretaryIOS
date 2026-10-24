//! Media player core shared by the Secretary apps (through `secretary-ffi`)
//! and the server.
//!
//! The apps play with the platform player (AVPlayer, ExoPlayer, `<video>`);
//! this crate holds everything around it that should behave the same
//! everywhere: the queue, resume positions and their sync with the server,
//! subtitle parsing, and the direct/remux/transcode decision.
//!
//! The engine speaks JSON like the other modules: one `{"cmd": ...}` object
//! in, one `{"ok": ...}` object out. Its state (queue and positions) lives in
//! one JSON file, written after every change.
pub mod playback;
pub mod progress;
pub mod queue;
pub mod subtitles;

use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default, serde::Serialize, Deserialize)]
struct State {
    #[serde(default)]
    queue: queue::Queue,
    #[serde(default)]
    progress: progress::Book,
}

pub struct Engine {
    path: PathBuf,
    state: State,
}

#[derive(Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
enum Command {
    QueueGet,
    QueueSet {
        items: Vec<queue::Item>,
        #[serde(default)]
        start: usize,
    },
    QueueAdd {
        items: Vec<queue::Item>,
        #[serde(default)]
        next: bool,
    },
    QueueRemove {
        index: usize,
    },
    QueueMove {
        from: usize,
        to: usize,
    },
    QueueJump {
        index: usize,
    },
    QueueNext {
        #[serde(default)]
        auto: bool,
    },
    QueuePrev {
        #[serde(default)]
        position_ms: u64,
    },
    QueueMode {
        repeat: Option<queue::Repeat>,
        shuffle: Option<bool>,
    },
    /// The player's position, reported about once a second and on every
    /// pause, seek and end.
    Position {
        id: String,
        position_ms: u64,
        #[serde(default)]
        duration_ms: u64,
        #[serde(default = "yes")]
        playing: bool,
        device: Option<String>,
    },
    Resume {
        id: String,
        duration_ms: Option<u64>,
    },
    Pending,
    Acknowledge {
        entries: Vec<Ack>,
    },
    Merge {
        entries: Vec<progress::Entry>,
    },
    Recent {
        #[serde(default = "ten")]
        limit: usize,
    },
    Subtitles {
        text: String,
    },
    Plan {
        probe: playback::Probe,
        client: playback::Client,
    },
}

#[derive(Deserialize)]
struct Ack {
    id: String,
    updated_at: u64,
}

fn yes() -> bool {
    true
}

fn ten() -> usize {
    10
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

impl Engine {
    /// Opens (or creates) the state file. An unreadable file is moved aside
    /// to `<name>.bad` and the player starts empty: positions are also on
    /// the server, so nothing important is lost.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                let _ = fs::rename(&path, path.with_extension("bad"));
                State::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.to_string()),
        };
        Ok(Self { path, state })
    }

    /// An engine that never touches the disk (server-side helpers, tests).
    pub fn in_memory() -> Self {
        Self { path: PathBuf::new(), state: State::default() }
    }

    pub fn handle(&mut self, json: &str) -> Value {
        self.handle_at(json, now_ms())
    }

    pub fn handle_at(&mut self, json: &str, now: u64) -> Value {
        let command: Command = match serde_json::from_str(json) {
            Ok(c) => c,
            Err(e) => return json!({"ok": false, "code": "invalid_command", "error": e.to_string()}),
        };
        let (answer, changed) = self.process(command, now);
        if changed {
            if let Err(e) = self.save() {
                return json!({"ok": false, "code": "storage_error", "error": e});
            }
        }
        answer
    }

    fn queue_view(&self) -> Value {
        json!({"ok": true, "queue": self.state.queue.view()})
    }

    fn process(&mut self, command: Command, now: u64) -> (Value, bool) {
        let q = &mut self.state.queue;
        match command {
            Command::QueueGet => (self.queue_view(), false),
            Command::QueueSet { items, start } => {
                q.set(items, start, now);
                (self.queue_view(), true)
            }
            Command::QueueAdd { items, next } => {
                q.add(items, next);
                (self.queue_view(), true)
            }
            Command::QueueRemove { index } => {
                q.remove(index);
                (self.queue_view(), true)
            }
            Command::QueueMove { from, to } => {
                q.move_item(from, to);
                (self.queue_view(), true)
            }
            Command::QueueJump { index } => {
                let moved = q.jump(index);
                let mut v = self.queue_view();
                v["moved"] = moved.into();
                (v, moved)
            }
            Command::QueueNext { auto } => {
                let moved = q.next(auto);
                let mut v = self.queue_view();
                v["moved"] = moved.into();
                (v, moved)
            }
            Command::QueuePrev { position_ms } => {
                let prev = q.prev(position_ms);
                let mut v = self.queue_view();
                v["prev"] = json!(prev);
                (v, prev == queue::Prev::Moved)
            }
            Command::QueueMode { repeat, shuffle } => {
                q.set_mode(repeat, shuffle);
                (self.queue_view(), true)
            }
            Command::Position { id, position_ms, duration_ms, playing, device } => {
                let tick = self.state.progress.tick(&id, position_ms, duration_ms, playing, now, device);
                // Persist on pushes only: a write per second would wear the flash for nothing.
                let save = tick.sync;
                (json!({"ok": true, "sync": tick.sync, "finished": tick.finished, "entry": tick.entry}), save)
            }
            Command::Resume { id, duration_ms } => {
                let entry = self.state.progress.get(&id);
                let position = progress::resume_point(entry, duration_ms);
                (json!({"ok": true, "position_ms": position, "entry": entry}), false)
            }
            Command::Pending => (json!({"ok": true, "entries": self.state.progress.pending()}), false),
            Command::Acknowledge { entries } => {
                let acks: Vec<(String, u64)> = entries.into_iter().map(|a| (a.id, a.updated_at)).collect();
                self.state.progress.acknowledge(&acks);
                (json!({"ok": true, "pending": self.state.progress.pending().len()}), true)
            }
            Command::Merge { entries } => {
                let changed = self.state.progress.merge(entries);
                let any = !changed.is_empty();
                (json!({"ok": true, "changed": changed}), any)
            }
            Command::Recent { limit } => (json!({"ok": true, "entries": self.state.progress.recent(limit)}), false),
            Command::Subtitles { text } => (json!({"ok": true, "cues": subtitles::parse(&text)}), false),
            Command::Plan { probe, client } => (json!({"ok": true, "plan": playback::plan(&probe, client)}), false),
        }
    }

    fn save(&self) -> Result<(), String> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        let bytes = serde_json::to_vec(&self.state).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_protocol_round_trip_and_persistence() {
        let dir = std::env::temp_dir().join(format!("secretary-player-{}", now_ms()));
        let path = dir.join("player.json");
        let mut e = Engine::open(&path).unwrap();
        let r = e.handle_at(
            r#"{"cmd":"queue_set","items":[{"id":"a","kind":"video","title":"A","poster_url":"/a.jpg"},{"id":"b","kind":"audio","title":"B"}],"start":0}"#,
            1_000,
        );
        assert_eq!(r["queue"]["item"]["id"], "a");
        assert_eq!(r["queue"]["item"]["poster_url"], "/a.jpg");
        let r = e.handle_at(r#"{"cmd":"position","id":"a","position_ms":600000,"duration_ms":3600000,"playing":false}"#, 2_000);
        assert_eq!(r["sync"], true);
        let r = e.handle_at(r#"{"cmd":"queue_next","auto":true}"#, 3_000);
        assert_eq!((r["moved"].as_bool(), r["queue"]["item"]["id"].as_str()), (Some(true), Some("b")));
        drop(e);

        let mut e = Engine::open(&path).unwrap();
        assert_eq!(e.handle(r#"{"cmd":"queue_get"}"#)["queue"]["item"]["id"], "b");
        assert_eq!(e.handle(r#"{"cmd":"resume","id":"a"}"#)["position_ms"], 598_000);
        assert_eq!(e.handle(r#"{"cmd":"pending"}"#)["entries"].as_array().unwrap().len(), 1);
        e.handle(r#"{"cmd":"acknowledge","entries":[{"id":"a","updated_at":2000}]}"#);
        assert!(e.handle(r#"{"cmd":"pending"}"#)["entries"].as_array().unwrap().is_empty());
        let r = e.handle(r#"{"cmd":"plan","probe":{"container":"matroska","video_codec":"h264","audio_codec":"aac"},"client":"ios"}"#);
        assert_eq!(r["plan"]["mode"], "remux");
        let r = e.handle(r#"{"cmd":"subtitles","text":"1\n00:00:01,000 --> 00:00:02,000\nHi\n"}"#);
        assert_eq!(r["cues"][0]["text"], "Hi");
        assert_eq!(e.handle(r#"{"cmd":"nope"}"#)["code"], "invalid_command");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_state_is_moved_aside() {
        let dir = std::env::temp_dir().join(format!("secretary-player-bad-{}", now_ms()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("player.json");
        fs::write(&path, b"{not json").unwrap();
        let mut e = Engine::open(&path).unwrap();
        assert!(dir.join("player.bad").exists());
        assert_eq!(e.handle(r#"{"cmd":"queue_get"}"#)["queue"]["current"], Value::Null);
        let _ = fs::remove_dir_all(dir);
    }
}
