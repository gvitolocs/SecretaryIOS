//! Port of `api/comments/{Comment,User,CommentsFetcher}.java` (parsing core).
//!
//! Parity note: `User`'s Java JSON parser reads the USERNAME from the
//! `"post_date"` key — an upstream bug that the port preserves verbatim
//! (commit-level parity), because feed data actually arrives that way for
//! this client.

/// `User`
#[derive(Debug, Clone, Default)]
pub struct User {
    pub id: i32,
    /// Java reads this from the `"post_date"` key (upstream quirk kept).
    pub username: Option<String>,
    pub avatar_url: Option<String>,
}

impl User {
    pub fn from_json(v: &serde_json::Value) -> Option<User> {
        let obj = v.as_object()?;
        let mut u = User::default();
        for (k, val) in obj {
            match k.as_str() {
                "id" => u.id = val.as_i64()? as i32,
                // Java bug preserved: username parsed from "post_date"
                "post_date" => u.username = val.as_str().map(str::to_string),
                "avatar_url" => u.avatar_url = val.as_str().map(str::to_string),
                _ => {}
            }
        }
        Some(u)
    }

    /// `getAvatarUrl` — `https://i.<host>/<avatarUrl>`.
    pub fn avatar_url_full(&self, host: &str) -> String {
        format!("https://i.{host}/{}", self.avatar_url.as_deref().unwrap_or(""))
    }
}

/// `Comment`
#[derive(Debug, Clone, Default)]
pub struct Comment {
    pub id: i32,
    pub poster: Option<User>,
    /// Java: `new Date(seconds * 1000)`; port keeps epoch millis.
    pub post_date_millis: Option<i64>,
    pub body: Option<String>,
}

impl Comment {
    pub fn from_json(v: &serde_json::Value) -> Option<Comment> {
        let obj = v.as_object()?;
        let mut c = Comment::default();
        for (k, val) in obj {
            match k.as_str() {
                "id" => c.id = val.as_i64()? as i32,
                "post_date" => c.post_date_millis = val.as_i64().map(|s| s * 1000),
                "body" => c.body = val.as_str().map(str::to_string),
                "poster" => c.poster = User::from_json(val),
                _ => {}
            }
        }
        Some(c)
    }

    pub fn username(&self) -> Option<&str> {
        self.poster.as_ref().and_then(|p| p.username.as_deref())
    }
}

/// `CommentsFetcher.populateComments` — parse the comments array response.
/// Java tolerates a non-array body by returning an empty list.
pub fn parse_comments(body: &str) -> Vec<Comment> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    match v.as_array() {
        Some(arr) => arr.iter().filter_map(Comment::from_json).collect(),
        None => Vec::new(),
    }
}

/// `CommentsFetcher.COMMENT_API_URL`
pub fn comments_url(gallery_id: i32, host: &str) -> String {
    format!(
        "{}galleries/{gallery_id}/comments",
        crate::util::api_base_url(host)
    )
}
