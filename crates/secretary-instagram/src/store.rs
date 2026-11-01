//! What is known about each profile, on disk: the profile, the highlight tray and, per feed (posts,
//! reels, stories, a highlight's items), every item seen so far, newest first. Pages are answered
//! from here at once; a refresh behind the answer only adds what is new.
//!
//! One JSON file (0600) per profile or feed under `<cache>/profiles/`, named by a hash so the
//! directory lists no account names. These files are never evicted. Reads come from memory after
//! the first; writes go through to disk.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use instarust_core::{HighlightSummary, MediaItem, UserProfile};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A feed keeps at most this many items, the oldest go first.
const MAX_FEED_ITEMS: usize = 5000;
/// Stories no longer up that a feed remembers.
const MAX_EXPIRED_STORIES: usize = 500;
/// Entries held in memory; past it the map starts over and reloads from disk as it is asked.
const MAX_IN_MEMORY: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedKind {
    Posts,
    Reels,
    Stories,
    Highlight,
}

impl FeedKind {
    fn name(self) -> &'static str {
        match self {
            FeedKind::Posts => "posts",
            FeedKind::Reels => "reels",
            FeedKind::Stories => "stories",
            FeedKind::Highlight => "highlight",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Feed {
    pub items: Vec<MediaItem>,
    /// Unix seconds of the last answer folded in; 0 when the backend was never asked.
    pub fetched_at: i64,
    /// Where the items older than the oldest known one start at the backend; `None` at the end.
    pub next_cursor: Option<String>,
    /// Stories: the ids in the last answer (the ones still up).
    pub live: Vec<String>,
}

/// Which URL an item keeps: the one whose file is on disk, else the newer one (a fresh backend URL
/// can name another rendition of the same picture, and an old CDN link stops working).
fn fresher(old: &str, new: &str, local: &dyn Fn(&str) -> bool) -> String {
    if old != new && local(old) {
        old.to_string()
    } else {
        new.to_string()
    }
}

/// A known item seen again: its counts and caption are the new ones, its URLs per [`fresher`].
fn update_known(old: &mut MediaItem, new: &MediaItem, local: &dyn Fn(&str) -> bool) {
    let media_url = fresher(&old.media_url, &new.media_url, local);
    let thumbnail_url = fresher(&old.thumbnail_url, &new.thumbnail_url, local);
    let carousel_urls = if old.carousel_urls.len() == new.carousel_urls.len() {
        old.carousel_urls
            .iter()
            .zip(&new.carousel_urls)
            .map(|(o, n)| fresher(o, n, local))
            .collect()
    } else {
        new.carousel_urls.clone()
    };
    *old = MediaItem {
        media_url,
        thumbnail_url,
        carousel_urls,
        ..new.clone()
    };
}

impl Feed {
    fn folded(&mut self, page: &[MediaItem], local: &dyn Fn(&str) -> bool) -> Vec<MediaItem> {
        let known: HashMap<String, usize> = self
            .items
            .iter()
            .enumerate()
            .map(|(i, m)| (m.id.clone(), i))
            .collect();
        let mut fresh = Vec::new();
        let mut seen = HashSet::new();
        for m in page {
            if !seen.insert(m.id.as_str()) {
                continue;
            }
            match known.get(&m.id) {
                Some(&i) => update_known(&mut self.items[i], m, local),
                None => fresh.push(m.clone()),
            }
        }
        fresh
    }

    /// A refresh's answer: what is new goes in front, in the backend's order. Returns how many.
    pub fn merge_front(&mut self, page: &[MediaItem], local: &dyn Fn(&str) -> bool) -> usize {
        let mut fresh = self.folded(page, local);
        let added = fresh.len();
        fresh.append(&mut self.items);
        fresh.truncate(MAX_FEED_ITEMS);
        self.items = fresh;
        added
    }

    /// An older page: what is new goes behind. Returns how many.
    pub fn append_older(&mut self, page: &[MediaItem], local: &dyn Fn(&str) -> bool) -> usize {
        let mut fresh = self.folded(page, local);
        let added = fresh.len();
        self.items.append(&mut fresh);
        self.items.truncate(MAX_FEED_ITEMS);
        added
    }

    /// The stories up now, in the backend's order, then the ones that expired since (newest
    /// first): those keep playing from the local copy.
    pub fn set_stories(&mut self, page: &[MediaItem], local: &dyn Fn(&str) -> bool) {
        self.folded(page, local);
        let live: HashSet<&str> = page.iter().map(|m| m.id.as_str()).collect();
        let mut gone: Vec<MediaItem> = self
            .items
            .iter()
            .filter(|m| !live.contains(m.id.as_str()))
            .cloned()
            .collect();
        gone.sort_by_key(|m| std::cmp::Reverse(m.taken_at));
        gone.truncate(MAX_EXPIRED_STORIES);
        let mut up: Vec<MediaItem> = Vec::with_capacity(page.len() + gone.len());
        let mut seen = HashSet::new();
        for m in page {
            if seen.insert(m.id.as_str()) {
                let kept = self.items.iter().find(|o| o.id == m.id);
                up.push(kept.cloned().unwrap_or_else(|| m.clone()));
            }
        }
        self.live = up.iter().map(|m| m.id.clone()).collect();
        up.append(&mut gone);
        self.items = up;
    }

    pub fn is_live(&self, id: &str) -> bool {
        self.live.iter().any(|l| l == id)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileDoc {
    pub profile: Option<UserProfile>,
    /// Unix seconds of the last bundle.
    pub fetched_at: i64,
    pub highlights: Vec<HighlightSummary>,
    pub highlights_at: i64,
    /// The backend answered 404 since it last answered: what is here is what is left.
    pub gone: bool,
}

pub struct Store {
    dir: PathBuf,
    feeds: Mutex<HashMap<String, Arc<Feed>>>,
    profiles: Mutex<HashMap<String, Arc<ProfileDoc>>>,
    /// Read, change and write one at a time; readers never wait for the disk.
    write: Mutex<()>,
}

fn load<T: DeserializeOwned + Default>(path: &PathBuf) -> T {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn cached<T: DeserializeOwned + Default>(
    map: &Mutex<HashMap<String, Arc<T>>>,
    key: &str,
    path: PathBuf,
) -> Arc<T> {
    if let Some(v) = map.lock().unwrap_or_else(|e| e.into_inner()).get(key) {
        return v.clone();
    }
    let v: Arc<T> = Arc::new(load(&path));
    let mut g = map.lock().unwrap_or_else(|e| e.into_inner());
    if g.len() >= MAX_IN_MEMORY {
        g.clear();
    }
    g.insert(key.to_string(), v.clone());
    v
}

impl Store {
    pub fn open(dir: PathBuf) -> std::io::Result<Store> {
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Store {
            dir,
            feeds: Mutex::new(HashMap::new()),
            profiles: Mutex::new(HashMap::new()),
            write: Mutex::new(()),
        })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!(
            "{}.json",
            hex::encode(&Sha256::digest(name.as_bytes())[..16])
        ))
    }

    pub fn feed(&self, kind: FeedKind, owner: &str) -> Arc<Feed> {
        let key = format!("{}:{owner}", kind.name());
        let path = self.path(&key);
        cached(&self.feeds, &key, path)
    }

    pub fn profile(&self, user: &str) -> Arc<ProfileDoc> {
        let key = format!("profile:{user}");
        let path = self.path(&key);
        cached(&self.profiles, &key, path)
    }

    pub fn update_feed<R>(&self, kind: FeedKind, owner: &str, f: impl FnOnce(&mut Feed) -> R) -> R {
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut feed = (*self.feed(kind, owner)).clone();
        let r = f(&mut feed);
        let key = format!("{}:{owner}", kind.name());
        self.save(&key, &feed);
        self.feeds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Arc::new(feed));
        r
    }

    pub fn update_profile<R>(&self, user: &str, f: impl FnOnce(&mut ProfileDoc) -> R) -> R {
        let _w = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let mut doc = (*self.profile(user)).clone();
        let r = f(&mut doc);
        let key = format!("profile:{user}");
        self.save(&key, &doc);
        self.profiles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, Arc::new(doc));
        r
    }

    fn save<T: Serialize>(&self, key: &str, value: &T) {
        if let Ok(bytes) = serde_json::to_vec(value) {
            crate::cache::write_private_atomic(&self.path(key), &bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, url: &str) -> MediaItem {
        MediaItem {
            id: id.into(),
            media_type: "image".into(),
            thumbnail_url: format!("{url}_t"),
            media_url: url.into(),
            caption: None,
            taken_at: Some(100),
            like_count: 1,
            comment_count: 0,
            is_video: false,
            carousel_urls: vec![],
        }
    }

    fn ids(f: &Feed) -> Vec<&str> {
        f.items.iter().map(|m| m.id.as_str()).collect()
    }

    fn none(_: &str) -> bool {
        false
    }

    #[test]
    fn what_is_new_goes_in_front_and_nothing_is_repeated() {
        let mut f = Feed::default();
        assert_eq!(f.merge_front(&[item("b", "u"), item("a", "u")], &none), 2);
        assert_eq!(f.merge_front(&[item("c", "u"), item("b", "u")], &none), 1);
        assert_eq!(ids(&f), ["c", "b", "a"]);
        assert_eq!(f.append_older(&[item("a", "u"), item("z", "u")], &none), 1);
        assert_eq!(ids(&f), ["c", "b", "a", "z"]);
        assert_eq!(f.merge_front(&[item("d", "u"), item("d", "u")], &none), 1);
        assert_eq!(ids(&f), ["d", "c", "b", "a", "z"]);
    }

    #[test]
    fn a_known_item_keeps_the_url_whose_file_is_on_disk() {
        let mut f = Feed::default();
        f.merge_front(&[item("a", "old")], &none);
        let mut newer = item("a", "new");
        newer.like_count = 9;
        // Nothing on disk: the newer link is the one that still works.
        f.merge_front(&[newer.clone()], &none);
        assert_eq!(f.items[0].media_url, "new");
        // The file of `new` is on disk: a later URL does not replace it; the counts do change.
        let mut later = item("a", "later");
        later.like_count = 12;
        f.merge_front(&[later], &|u| u == "new");
        assert_eq!(f.items[0].media_url, "new");
        assert_eq!(f.items[0].thumbnail_url, "later_t");
        assert_eq!(f.items[0].like_count, 12);
    }

    #[test]
    fn stories_that_went_stay_behind_the_ones_up_now() {
        let mut f = Feed::default();
        let mut older = item("s1", "u");
        older.taken_at = Some(10);
        let mut newer = item("s2", "u");
        newer.taken_at = Some(20);
        f.set_stories(&[older, newer], &none);
        assert!(f.is_live("s1") && f.is_live("s2"));
        f.set_stories(&[item("s3", "u")], &none);
        assert_eq!(ids(&f), ["s3", "s2", "s1"]);
        assert!(f.is_live("s3"));
        assert!(!f.is_live("s1"));
        // An empty answer: nothing is up, nothing is forgotten.
        f.set_stories(&[], &none);
        assert_eq!(f.items.len(), 3);
        assert!(f.live.is_empty());
    }

    #[test]
    fn feeds_and_profiles_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open(dir.path().join("p")).unwrap();
        s.update_feed(FeedKind::Reels, "natgeo", |f| {
            f.merge_front(&[item("a", "u")], &none);
            f.next_cursor = Some("CUR".into());
        });
        s.update_profile("natgeo", |d| d.fetched_at = 5);
        let again = Store::open(dir.path().join("p")).unwrap();
        assert_eq!(ids(&again.feed(FeedKind::Reels, "natgeo")), ["a"]);
        assert_eq!(
            again.feed(FeedKind::Reels, "natgeo").next_cursor.as_deref(),
            Some("CUR")
        );
        assert_eq!(again.profile("natgeo").fetched_at, 5);
        assert!(again.feed(FeedKind::Posts, "natgeo").items.is_empty());
        // The directory names no account.
        for e in std::fs::read_dir(dir.path().join("p")).unwrap().flatten() {
            assert!(!e.file_name().to_string_lossy().contains("natgeo"));
        }
    }
}
