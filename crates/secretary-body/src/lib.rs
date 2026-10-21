//! Body-weight tracking: the same command protocol, replay protection and
//! atomic persistence as `secretary-gym` (op_id receipts, expected_revision,
//! candidate-then-swap). Integer grams only in stored state.
pub mod kernel;
pub mod models;
pub mod stats;
pub mod units;
pub use models::*;

use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

pub const MAX_INPUT: usize = 256 * 1024;
pub const MIN_WEIGHT: u64 = 1_000;
pub const MAX_WEIGHT: u64 = 650_000;
pub const MAX_NOTE: usize = 1_000;
pub const MAX_ENTRIES: usize = 50_000;
pub const MAX_RECEIPTS: usize = 100_000;
/// Entries may be dated at most this far after the server/device clock.
pub const MAX_FUTURE_MS: u64 = stats::DAY_MS;
const MAX_TIME: u64 = 253_402_300_799_000;
type Result<T> = std::result::Result<T, String>;

pub struct Engine {
    state: State,
    path: Option<PathBuf>,
    _lock: Option<File>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(MAX_TIME as u128) as u64
}

impl Engine {
    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn in_memory() -> Self {
        Self {
            state: State::initial(),
            path: None,
            _lock: None,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock()
            .map_err(|_| "This body data file is already open in another process".to_string())?;
        let state = if path.exists() {
            let bytes = fs::read(&path).map_err(|e| e.to_string())?;
            serde_json::from_slice::<State>(&bytes)
                .map_err(|e| format!("Saved data could not be read: {e}"))?
        } else {
            State::initial()
        };
        validate(&state)?;
        Ok(Self {
            state,
            path: Some(path),
            _lock: Some(lock),
        })
    }

    pub fn handle(&mut self, json: &str) -> Response {
        self.handle_at(json, now_ms())
    }

    pub fn handle_at(&mut self, json: &str, now: u64) -> Response {
        match self.process(json, now) {
            Ok(r) => r,
            Err((code, error)) => Response {
                ok: false,
                replayed: false,
                error: Some(error),
                code: Some(code.into()),
                ..Default::default()
            },
        }
    }

    fn process(
        &mut self,
        json: &str,
        now: u64,
    ) -> std::result::Result<Response, (&'static str, String)> {
        let invalid = |e: String| ("invalid_command", e);
        if json.len() > MAX_INPUT || now > MAX_TIME {
            return Err(invalid(
                "Request or timestamp exceeds supported bounds".into(),
            ));
        }
        let req: Request = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
        // The timezone is part of the shared envelope; body statistics use
        // rolling 7/30/90-day windows, so it is validated but not needed.
        req.timezone
            .parse::<chrono_tz::Tz>()
            .map_err(|_| invalid("Unknown IANA timezone".into()))?;
        if let Command::ParseWeight { value, unit } = &req.command {
            let unit = unit.as_deref().unwrap_or(&self.state.settings.unit);
            return Ok(Response {
                ok: true,
                weight_grams: Some(units::parse_weight(value, unit).map_err(invalid)?),
                ..Default::default()
            });
        }
        let mut replayed = false;
        if !req.command.is_query() {
            let op = req
                .op_id
                .filter(|id| !id.trim().is_empty() && id.len() <= 128)
                .ok_or_else(|| {
                    invalid("A nonempty op_id (at most 128 bytes) is required".into())
                })?;
            let fingerprint = format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&req.command).map_err(|e| invalid(e.to_string()))?
                )
            );
            if let Some(previous) = self.state.receipts.get(&op) {
                if previous != &fingerprint {
                    return Err((
                        "op_id_reused",
                        "This operation ID was already used for a different command".into(),
                    ));
                }
                replayed = true;
            } else {
                if req.expected_revision != Some(self.state.revision) {
                    return Err((
                        "conflict",
                        "State changed. Refresh and retry the edit.".into(),
                    ));
                }
                // Rejection never mutates live state: only a fully validated,
                // durably written candidate becomes the new state.
                let mut candidate = self.state.clone();
                apply(&mut candidate, req.command, now).map_err(invalid)?;
                candidate.revision = self
                    .state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| invalid("Revision limit reached".into()))?;
                candidate.receipts.insert(op, fingerprint);
                validate(&candidate).map_err(invalid)?;
                self.persist(&candidate).map_err(|e| ("storage_error", e))?;
                self.state = candidate;
            }
        }
        Ok(Response {
            ok: true,
            replayed,
            view: Some(view(&self.state, now)),
            ..Default::default()
        })
    }

    fn persist(&self, state: &State) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
            serde_json::to_writer(&mut file, state).map_err(|e| e.to_string())?;
            file.flush().map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            fs::rename(&tmp, path).map_err(|e| e.to_string())?;
            // Best effort: some mobile filesystems do not support directory fsync.
            if let Some(parent) = path.parent() {
                if let Ok(dir) = File::open(parent) {
                    let _ = dir.sync_all();
                }
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(tmp);
        }
        result
    }
}

/// Exactly one (`required`) or at most one of `weight_grams` / `weight`.
fn weight_input(
    grams: Option<u64>,
    text: Option<&str>,
    unit: &str,
    required: bool,
) -> Result<Option<u64>> {
    match (grams, text) {
        (Some(_), Some(_)) => Err("Give either weight_grams or weight, not both".into()),
        (Some(g), None) => Ok(Some(g)),
        (None, Some(t)) => units::parse_weight(t, unit).map(Some),
        (None, None) if required => Err("A weight (weight_grams or weight) is required".into()),
        (None, None) => Ok(None),
    }
}

fn check_measured(measured_ms: u64, now: u64) -> Result<u64> {
    if measured_ms > now.saturating_add(MAX_FUTURE_MS) || measured_ms > MAX_TIME {
        return Err("The measurement time is too far in the future".into());
    }
    Ok(measured_ms)
}

fn apply(s: &mut State, command: Command, now: u64) -> Result<()> {
    match command {
        Command::LogEntry {
            weight_grams,
            weight,
            measured_ms,
            note,
        } => {
            let grams = weight_input(weight_grams, weight.as_deref(), &s.settings.unit, true)?
                .expect("required weight");
            s.entries.push(Entry {
                id: Uuid::new_v4().to_string(),
                weight_grams: grams,
                measured_ms: check_measured(measured_ms.unwrap_or(now), now)?,
                note: note.unwrap_or_default().trim().to_string(),
                created_ms: now,
                updated_ms: now,
            });
        }
        Command::EditEntry {
            entry_id,
            weight_grams,
            weight,
            measured_ms,
            note,
        } => {
            if weight_grams.is_none() && weight.is_none() && measured_ms.is_none() && note.is_none()
            {
                return Err("Nothing to change".into());
            }
            let grams = weight_input(weight_grams, weight.as_deref(), &s.settings.unit, false)?;
            let measured = measured_ms.map(|m| check_measured(m, now)).transpose()?;
            let entry = s
                .entries
                .iter_mut()
                .find(|e| e.id == entry_id)
                .ok_or("Entry not found")?;
            if let Some(g) = grams {
                entry.weight_grams = g;
            }
            if let Some(m) = measured {
                entry.measured_ms = m;
            }
            if let Some(n) = note {
                entry.note = n.trim().to_string();
            }
            entry.updated_ms = now;
        }
        Command::DeleteEntry { entry_id } => {
            let pos = s
                .entries
                .iter()
                .position(|e| e.id == entry_id)
                .ok_or("Entry not found")?;
            s.entries.remove(pos);
        }
        Command::UpdateSettings {
            unit,
            goal_grams,
            goal,
        } => {
            if unit != "kg" && unit != "lb" {
                return Err("Unit must be kg or lb".into());
            }
            let goal_grams = weight_input(goal_grams, goal.as_deref(), &unit, false)?;
            s.settings = Settings { unit, goal_grams };
        }
        Command::Snapshot | Command::ParseWeight { .. } => {
            return Err("Queries cannot mutate state".into())
        }
    }
    Ok(())
}

pub fn validate(s: &State) -> Result<()> {
    if s.schema_version != 1 {
        return Err("Unsupported body data schema version".into());
    }
    if s.settings.unit != "kg" && s.settings.unit != "lb" {
        return Err("Unit must be kg or lb".into());
    }
    let in_range = |g: u64| (MIN_WEIGHT..=MAX_WEIGHT).contains(&g);
    if s.settings.goal_grams.is_some_and(|g| !in_range(g)) {
        return Err("Goal weight must be between 1 and 650 kg".into());
    }
    if s.entries.len() > MAX_ENTRIES || s.receipts.len() > MAX_RECEIPTS {
        return Err("Data capacity reached; export or archive old entries first".into());
    }
    let mut ids = HashSet::new();
    for e in &s.entries {
        if e.id.is_empty() || e.id.len() > 128 || !ids.insert(e.id.as_str()) {
            return Err("Empty or duplicate entry ID".into());
        }
        if !in_range(e.weight_grams) {
            return Err("Weight must be between 1 and 650 kg".into());
        }
        if e.note.len() > MAX_NOTE {
            return Err("Notes are limited to 1,000 bytes".into());
        }
        if e.measured_ms > MAX_TIME || e.created_ms > MAX_TIME || e.updated_ms > MAX_TIME {
            return Err("Entry time is outside the supported range".into());
        }
    }
    Ok(())
}

/// The snapshot view: entries newest first, stats, series and labels.
pub fn view(state: &State, now: u64) -> View {
    let unit = state.settings.unit.as_str();
    let mut entries = state.entries.clone();
    entries.sort_by(|a, b| (b.measured_ms, &b.id).cmp(&(a.measured_ms, &a.id)));
    let stats = stats::stats(&state.entries, state.settings.goal_grams, now);
    let labels = entries
        .iter()
        .map(|e| (e.id.clone(), units::label(e.weight_grams, unit)))
        .collect();
    View {
        revision: state.revision,
        settings: state.settings.clone(),
        series: stats::Timeline::new(&state.entries).series(),
        stat_labels: stat_labels(&stats, unit),
        stats,
        entries,
        labels,
    }
}

pub fn stat_labels(s: &stats::Stats, unit: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let plain = [
        ("current", s.current_grams),
        ("avg7", s.avg7_grams),
        ("min", s.min_grams),
        ("max", s.max_grams),
        ("goal", s.goal_grams),
    ];
    for (k, v) in plain {
        if let Some(g) = v {
            out.insert(k.to_string(), units::label(g, unit));
        }
    }
    let signed = [
        ("weekly_change", s.weekly_change_grams),
        ("trend30", s.trend30_grams),
        ("trend90", s.trend90_grams),
        ("goal_distance", s.goal_distance_grams),
    ];
    for (k, v) in signed {
        if let Some(d) = v {
            out.insert(k.to_string(), units::signed_label(d, unit));
        }
    }
    out
}

/// Body summary for Lucina.
pub fn insights(state: &State, now: u64) -> BodyInsights {
    let stats = stats::stats(&state.entries, state.settings.goal_grams, now);
    BodyInsights {
        unit: state.settings.unit.clone(),
        stat_labels: stat_labels(&stats, &state.settings.unit),
        stats,
    }
}
