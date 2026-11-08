//! A small TTL cache that zeroizes what it lets go.
//!
//! Every value that leaves the cache (expired, replaced, removed, cleared,
//! or the cache dropped) is zeroized explicitly before it is dropped.
//! Times are passed in (`now`) so tests need no sleeping.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::{Duration, Instant};

use zeroize::Zeroize;

pub const DEFAULT_TTL: Duration = Duration::from_secs(600);

struct Entry<V: Zeroize> {
    value: V,
    expires: Instant,
}

pub struct TtlCache<K: Eq + Hash + Clone, V: Zeroize> {
    ttl: Duration,
    map: HashMap<K, Entry<V>>,
}

impl<K: Eq + Hash + Clone, V: Zeroize> TtlCache<K, V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            map: HashMap::new(),
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// A live entry, or `None` (an expired one is zeroized and removed).
    pub fn get(&mut self, k: &K, now: Instant) -> Option<&V> {
        let expired = self.map.get(k).is_some_and(|e| e.expires <= now);
        if expired {
            self.remove(k);
            return None;
        }
        self.map.get(k).map(|e| &e.value)
    }

    pub fn insert(&mut self, k: K, value: V, now: Instant) {
        if let Some(mut old) = self.map.insert(
            k,
            Entry {
                value,
                expires: now + self.ttl,
            },
        ) {
            old.value.zeroize();
        }
    }

    pub fn remove(&mut self, k: &K) -> bool {
        match self.map.remove(k) {
            Some(mut e) => {
                e.value.zeroize();
                true
            }
            None => false,
        }
    }

    /// Zeroize and drop every expired entry. Returns how many.
    pub fn purge_expired(&mut self, now: Instant) -> usize {
        let dead: Vec<K> = self
            .map
            .iter()
            .filter(|(_, e)| e.expires <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &dead {
            self.remove(k);
        }
        dead.len()
    }

    pub fn clear(&mut self) {
        for (_, mut e) in self.map.drain() {
            e.value.zeroize();
        }
    }
}

impl<K: Eq + Hash + Clone, V: Zeroize> Drop for TtlCache<K, V> {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Counts zeroizations and wipes its own buffer.
    struct Probe {
        buf: Vec<u8>,
        wiped: Arc<AtomicUsize>,
    }

    impl Zeroize for Probe {
        fn zeroize(&mut self) {
            self.buf.zeroize();
            self.wiped.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn probe(w: &Arc<AtomicUsize>, s: &str) -> Probe {
        Probe {
            buf: s.as_bytes().to_vec(),
            wiped: w.clone(),
        }
    }

    #[test]
    fn ttl_expiry_and_zeroize_on_every_exit() {
        let w = Arc::new(AtomicUsize::new(0));
        let t0 = Instant::now();
        let mut c: TtlCache<&str, Probe> = TtlCache::new(Duration::from_secs(600));
        c.insert("a", probe(&w, "alpha-secret"), t0);
        assert_eq!(
            c.get(&"a", t0 + Duration::from_secs(599)).unwrap().buf,
            b"alpha-secret"
        );
        // expired at exactly the TTL
        assert!(c.get(&"a", t0 + Duration::from_secs(600)).is_none());
        assert_eq!(w.load(Ordering::SeqCst), 1);
        assert!(c.is_empty());
        // replaced
        c.insert("b", probe(&w, "beta-1"), t0);
        c.insert("b", probe(&w, "beta-2"), t0);
        assert_eq!(w.load(Ordering::SeqCst), 2);
        // purge
        c.insert("c", probe(&w, "gamma"), t0);
        assert_eq!(c.purge_expired(t0 + Duration::from_secs(601)), 2);
        assert_eq!(w.load(Ordering::SeqCst), 4);
        // drop
        c.insert("d", probe(&w, "delta"), t0);
        c.insert("e", probe(&w, "epsilon"), t0);
        drop(c);
        assert_eq!(w.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn the_wipe_really_clears_the_bytes() {
        let w = Arc::new(AtomicUsize::new(0));
        let mut p = probe(&w, "hunter2");
        p.zeroize();
        assert!(p.buf.iter().all(|b| *b == 0) || p.buf.is_empty());
        let mut s = String::from("hunter2");
        s.zeroize();
        assert!(s.is_empty());
    }
}
