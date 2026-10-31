//! Pure Node-compatible rendezvous decision, suitable for a Verus mirror.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LockRecord {
    pub holder: String,
    pub name: String,
    pub bot_key: String,
    pub hostname: String,
    pub pid: Option<u32>,
    pub pid_ns: String,
    pub instance: String,
    pub boot_id: String,
    pub started_at: String,
    pub heartbeat_at: String,
}
#[derive(Debug, Clone)]
pub struct Identity {
    pub holder: String,
    pub instance: String,
    pub pid_ns: String,
    pub pid: u32,
    /// Snapshot of the record PID's liveness, supplied by the impure caller.
    pub record_pid_alive: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Acquire,
    Defer,
}

pub fn is_record_live(record: &LockRecord, now_ms: i64, boot_id: &str) -> bool {
    let Ok(heartbeat) = chrono::DateTime::parse_from_rfc3339(&record.heartbeat_at) else {
        return false;
    };
    now_ms.saturating_sub(heartbeat.timestamp_millis()) <= 90_000
        && (record.boot_id.is_empty() || boot_id.is_empty() || record.boot_id == boot_id)
}
pub fn is_own_previous_run(record: &LockRecord, me: &Identity) -> bool {
    (!me.instance.is_empty() && record.instance == me.instance)
        || (!me.pid_ns.is_empty()
            && record.pid_ns == me.pid_ns
            && record
                .pid
                .is_some_and(|p| p == me.pid || !me.record_pid_alive))
}
pub fn lease_decision(
    record: Option<&LockRecord>,
    me: &Identity,
    now_ms: i64,
    boot_id: &str,
) -> Decision {
    if record.is_some_and(|r| {
        r.holder != me.holder && is_record_live(r, now_ms, boot_id) && !is_own_previous_run(r, me)
    }) {
        Decision::Defer
    } else {
        Decision::Acquire
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn node_liveness_cases() {
        let now = 1_800_000_000_000;
        let mut r = LockRecord {
            holder: "foreign".into(),
            boot_id: "a".into(),
            heartbeat_at: chrono::DateTime::from_timestamp_millis(now - 5000)
                .unwrap()
                .to_rfc3339(),
            ..Default::default()
        };
        let me = Identity {
            holder: "ours".into(),
            instance: "lucina-rs".into(),
            pid_ns: "ns:1".into(),
            pid: 5,
            record_pid_alive: true,
        };
        assert_eq!(lease_decision(None, &me, now, "a"), Decision::Acquire);
        assert_eq!(lease_decision(Some(&r), &me, now, "a"), Decision::Defer);
        assert!(!is_record_live(&r, now, "old"));
        assert!(!is_record_live(&r, now + 600_000, "a"));
        r.boot_id.clear();
        assert!(is_record_live(&r, now, "a"));
        r.heartbeat_at = "invalid".into();
        assert!(!is_record_live(&r, now, "a"));
    }
    #[test]
    fn node_previous_run_cases() {
        let mut me = Identity {
            holder: "ours".into(),
            instance: "lucina-nez".into(),
            pid_ns: "ns:1".into(),
            pid: 5,
            record_pid_alive: true,
        };
        let mut r = LockRecord {
            instance: "lucina-nez".into(),
            ..Default::default()
        };
        assert!(is_own_previous_run(&r, &me));
        r.instance = "other".into();
        assert!(!is_own_previous_run(&r, &me));
        me.instance.clear();
        r.pid_ns = "ns:1".into();
        r.pid = Some(77);
        me.record_pid_alive = false;
        assert!(is_own_previous_run(&r, &me));
        me.record_pid_alive = true;
        assert!(!is_own_previous_run(&r, &me));
        r.pid = Some(5);
        assert!(is_own_previous_run(&r, &me));
        r.pid_ns = "ns:2".into();
        me.record_pid_alive = false;
        assert!(!is_own_previous_run(&r, &me));
    }
}
