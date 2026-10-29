//! Voice profiles (`voices.json`, mode 0600, next to the memo data): one per named person,
//! the owner's from his microphone track, and the unknown voices heard so far.
//!
//! A diarized cluster is the profile whose closest stored embedding is the most similar,
//! when that similarity reaches the threshold (kernel MT5 `best_match`); otherwise the
//! closest unknown voice above the threshold, otherwise a new unknown voice. An unknown voice
//! heard in `ask_after` different recordings is asked about once (MT6 `should_ask`; the
//! `asked_at` marker is written before the notification).
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{cosine, mean_direction};
use crate::meetings::kernel;
use crate::store::{read_json, write_json};

/// Embeddings kept per profile (the most recent).
pub const MAX_EMBEDDINGS: usize = 12;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    /// `vp_` + 12 hex digits.
    pub id: String,
    pub name: String,
    /// The owner's own voice (from `mic.m4a`).
    #[serde(default)]
    pub owner: bool,
    pub embeddings: Vec<Vec<f32>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Where an unknown voice was heard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sighting {
    /// `mtg_…` or `mm_…`.
    pub source: String,
    /// The speaker id in that recording (`S2`).
    pub speaker: String,
    pub title: String,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Unknown {
    /// `vu_` + 12 hex digits.
    pub id: String,
    pub centroid: Vec<f32>,
    pub sightings: Vec<Sighting>,
    /// Set before the "who is this?" notification is sent (at most once).
    #[serde(default)]
    pub asked_at: Option<DateTime<Utc>>,
    /// The profile it became when the owner named it.
    #[serde(default)]
    pub named: Option<String>,
}

impl Unknown {
    /// Distinct recordings it was heard in.
    pub fn recordings(&self) -> u32 {
        let mut s: Vec<&str> = self.sightings.iter().map(|x| x.source.as_str()).collect();
        s.sort();
        s.dedup();
        s.len() as u32
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Book {
    #[serde(default)]
    pub profiles: Vec<Profile>,
    #[serde(default)]
    pub unknown: Vec<Unknown>,
}

/// Who a cluster is.
#[derive(Debug, Clone, PartialEq)]
pub enum Identity {
    Known {
        profile: String,
        name: String,
        owner: bool,
        similarity: f32,
    },
    Unknown {
        voice: String,
        similarity: Option<f32>,
    },
}

/// Cosine similarity as an integer in thousandths (the kernel compares integers).
pub fn permille(sim: f32) -> i32 {
    (sim.clamp(-1.0, 1.0) * 1000.0).round() as i32
}

fn new_id(prefix: &str) -> String {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).expect("system randomness");
    format!(
        "{prefix}{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    )
}

pub struct VoiceBook {
    path: PathBuf,
    book: Mutex<Book>,
}

impl VoiceBook {
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            book: Mutex::new(read_json(path)?.unwrap_or_default()),
        })
    }

    pub fn snapshot(&self) -> Book {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Applies `f` and saves (atomically); the in-memory copy changes only when the write did.
    fn update<T>(&self, f: impl FnOnce(&mut Book) -> T) -> std::io::Result<T> {
        let mut g = self.lock();
        let mut next = g.clone();
        let out = f(&mut next);
        write_json(&self.path, &next)?;
        *g = next;
        Ok(out)
    }

    /// The profile a voice belongs to, if any (MT5 over each profile's best similarity).
    pub fn best_profile(book: &Book, voice: &[f32], threshold: f32) -> Option<(usize, f32)> {
        let sims: Vec<f32> = book
            .profiles
            .iter()
            .map(|p| {
                p.embeddings
                    .iter()
                    .map(|e| cosine(voice, e))
                    .fold(-1.0, f32::max)
            })
            .collect();
        let scores: Vec<i32> = sims.iter().map(|s| permille(*s)).collect();
        kernel::best_match(&scores, permille(threshold)).map(|i| (i, sims[i]))
    }

    /// Identifies a cluster heard in `sighting.source`, recording the sighting of an unknown
    /// voice (idempotent per source and speaker, so a pipeline retry adds nothing).
    pub fn identify(
        &self,
        voice: &[f32],
        threshold: f32,
        sighting: Sighting,
    ) -> std::io::Result<Identity> {
        self.update(|b| {
            if let Some((i, sim)) = Self::best_profile(b, voice, threshold) {
                let p = &b.profiles[i];
                return Identity::Known {
                    profile: p.id.clone(),
                    name: p.name.clone(),
                    owner: p.owner,
                    similarity: sim,
                };
            }
            let open: Vec<usize> = (0..b.unknown.len())
                .filter(|i| b.unknown[*i].named.is_none())
                .collect();
            let sims: Vec<f32> = open
                .iter()
                .map(|i| cosine(voice, &b.unknown[*i].centroid))
                .collect();
            let scores: Vec<i32> = sims.iter().map(|s| permille(*s)).collect();
            let hit = kernel::best_match(&scores, permille(threshold));
            let (idx, sim) = match hit {
                Some(k) => (open[k], Some(sims[k])),
                None => {
                    b.unknown.push(Unknown {
                        id: new_id("vu_"),
                        centroid: voice.to_vec(),
                        sightings: Vec::new(),
                        asked_at: None,
                        named: None,
                    });
                    (b.unknown.len() - 1, None)
                }
            };
            let u = &mut b.unknown[idx];
            let seen = u
                .sightings
                .iter()
                .any(|s| s.source == sighting.source && s.speaker == sighting.speaker);
            if !seen {
                let n = u.sightings.len() as f32;
                // Running mean direction of every sighting.
                let mut c: Vec<f32> = u.centroid.iter().map(|x| x * n.max(1.0)).collect();
                if hit.is_some() && c.len() == voice.len() {
                    for (a, v) in c.iter_mut().zip(voice) {
                        *a += v;
                    }
                    super::normalize(&mut c);
                    u.centroid = c;
                }
                u.sightings.push(sighting);
            }
            Identity::Unknown {
                voice: u.id.clone(),
                similarity: sim,
            }
        })
    }

    /// Adds a voice sample to the named person's profile (created if new; names match
    /// case-insensitively). Returns the profile id.
    pub fn add_to_profile(
        &self,
        name: &str,
        owner: bool,
        voice: &[f32],
    ) -> std::io::Result<String> {
        let name = name.trim().to_owned();
        self.update(|b| {
            let now = Utc::now();
            let i = match b.profiles.iter().position(|p| {
                (owner && p.owner) || (!owner && !p.owner && p.name.eq_ignore_ascii_case(&name))
            }) {
                Some(i) => i,
                None => {
                    b.profiles.push(Profile {
                        id: new_id("vp_"),
                        name: name.clone(),
                        owner,
                        embeddings: Vec::new(),
                        created_at: now,
                        updated_at: now,
                    });
                    b.profiles.len() - 1
                }
            };
            let p = &mut b.profiles[i];
            p.name = name;
            if !voice.is_empty() {
                p.embeddings.push(voice.to_vec());
                let extra = p.embeddings.len().saturating_sub(MAX_EMBEDDINGS);
                p.embeddings.drain(..extra);
            }
            p.updated_at = now;
            p.id.clone()
        })
    }

    /// The owner names an unknown voice: its centroid joins the person's profile, and the
    /// recordings it was heard in are returned (their speakers get the name).
    pub fn name_unknown(
        &self,
        voice_id: &str,
        name: &str,
    ) -> std::io::Result<Option<(String, Vec<Sighting>)>> {
        let Some(u) = self
            .lock()
            .unknown
            .iter()
            .find(|u| u.id == voice_id)
            .cloned()
        else {
            return Ok(None);
        };
        let profile = self.add_to_profile(name, false, &u.centroid)?;
        let pid = profile.clone();
        self.update(|b| {
            if let Some(x) = b.unknown.iter_mut().find(|x| x.id == voice_id) {
                x.named = Some(pid);
            }
        })?;
        Ok(Some((profile, u.sightings)))
    }

    /// Unknown voices to ask about now (MT6), oldest first.
    pub fn to_ask(&self, ask_after: u32) -> Vec<Unknown> {
        self.lock()
            .unknown
            .iter()
            .filter(|u| {
                kernel::should_ask(
                    u.recordings(),
                    ask_after,
                    u.asked_at.is_some(),
                    u.named.is_some(),
                )
            })
            .cloned()
            .collect()
    }

    /// Records the question before it is sent; false when it was already asked.
    pub fn mark_asked(&self, voice_id: &str) -> std::io::Result<bool> {
        self.update(|b| match b.unknown.iter_mut().find(|u| u.id == voice_id) {
            Some(u) if u.asked_at.is_none() => {
                u.asked_at = Some(Utc::now());
                true
            }
            _ => false,
        })
    }

    /// The owner's voice profile, if any.
    pub fn owner_voice(&self) -> Option<Vec<f32>> {
        let b = self.lock();
        let p = b.profiles.iter().find(|p| p.owner)?;
        Some(mean_direction(
            &p.embeddings.iter().map(Vec::as_slice).collect::<Vec<_>>(),
        ))
        .filter(|v| !v.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sighting(source: &str, speaker: &str) -> Sighting {
        Sighting {
            source: source.into(),
            speaker: speaker.into(),
            title: "t".into(),
            at: Utc::now(),
        }
    }

    #[test]
    fn known_unknown_and_asking_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("voices.json");
        let book = VoiceBook::open(&path).unwrap();
        let anna = [1.0, 0.0, 0.0];
        let bo = [0.0, 1.0, 0.0];
        book.add_to_profile("Anna", false, &anna).unwrap();
        // Anna, a bit off: known (cos ≈ 0.95 ≥ 0.6).
        match book
            .identify(&[0.95, 0.3, 0.0], 0.6, sighting("mtg_1", "S1"))
            .unwrap()
        {
            Identity::Known {
                name, similarity, ..
            } => {
                assert_eq!(name, "Anna");
                assert!(similarity > 0.9);
            }
            other => panic!("{other:?}"),
        }
        // Bo is unknown; heard again in the same meeting (a retry) and in a second one.
        let first = book.identify(&bo, 0.6, sighting("mtg_1", "S2")).unwrap();
        let again = book.identify(&bo, 0.6, sighting("mtg_1", "S2")).unwrap();
        assert_eq!(
            first,
            Identity::Unknown {
                voice: match &first {
                    Identity::Unknown { voice, .. } => voice.clone(),
                    _ => unreachable!(),
                },
                similarity: None
            }
        );
        let Identity::Unknown { voice, .. } = again else {
            panic!()
        };
        assert_eq!(book.snapshot().unknown[0].sightings.len(), 1);
        assert!(book.to_ask(2).is_empty());
        book.identify(&[0.0, 0.98, 0.1], 0.6, sighting("mtg_2", "S1"))
            .unwrap();
        let ask = book.to_ask(2);
        assert_eq!(ask.len(), 1);
        assert_eq!(ask[0].id, voice);
        assert!(book.mark_asked(&voice).unwrap());
        assert!(!book.mark_asked(&voice).unwrap());
        assert!(book.to_ask(2).is_empty());
        // Naming it: a profile, and Bo is known from now on; it survives a reopen.
        let (pid, seen) = book.name_unknown(&voice, "Bo").unwrap().unwrap();
        assert_eq!(seen.len(), 2);
        drop(book);
        let book = VoiceBook::open(&path).unwrap();
        match book.identify(&bo, 0.6, sighting("mtg_3", "S1")).unwrap() {
            Identity::Known { profile, name, .. } => {
                assert_eq!((profile, name.as_str()), (pid, "Bo"))
            }
            other => panic!("{other:?}"),
        }
        assert!(book.owner_voice().is_none());
        book.add_to_profile("Giuseppe", true, &[0.0, 0.0, 1.0])
            .unwrap();
        assert_eq!(book.owner_voice().unwrap(), vec![0.0, 0.0, 1.0]);
    }

    #[test]
    fn below_the_threshold_is_a_new_voice() {
        let dir = tempfile::tempdir().unwrap();
        let book = VoiceBook::open(&dir.path().join("v.json")).unwrap();
        book.add_to_profile("Anna", false, &[1.0, 0.0]).unwrap();
        // cos(45°) ≈ 0.707: under 0.8, so not Anna.
        let id = book
            .identify(&[1.0, 1.0], 0.8, sighting("mm_1", "S1"))
            .unwrap();
        assert!(matches!(
            id,
            Identity::Unknown {
                similarity: None,
                ..
            }
        ));
        for _ in 0..20 {
            book.add_to_profile("anna", false, &[1.0, 0.0]).unwrap();
        }
        let b = book.snapshot();
        assert_eq!(b.profiles.len(), 1);
        assert_eq!(b.profiles[0].embeddings.len(), MAX_EMBEDDINGS);
    }
}
