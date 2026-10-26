//! Port of `api/components/Page.java` + `components/classes/Size.java`.

use crate::enums::ImageType;
use serde::{Deserialize, Serialize};

/// `Size`
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

impl Size {
    pub fn new(width: i32, height: i32) -> Size {
        Size { width, height }
    }
}

/// `Page` — paths are URL strings (Java used `android.net.Uri`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Page {
    /// zero-based index; `0` for cover/thumbnail (Java `Page()` default)
    pub page: i32,
    pub image_type: ImageType,
    pub path: String,
    pub thumb_path: Option<String>,
    pub size: Size,
}

impl Page {
    /// `Page(ImageType, JsonReader, page)` — recognizes `path`, `thumbnail`,
    /// `width`, `height`; unknown keys skipped. `path` is prefixed
    /// `https://<i1|t1>.<host>/`, `thumbnail` always `t1`.
    pub fn from_json(v: &serde_json::Value, image_type: ImageType, page: i32, host: &str) -> Page {
        let mut out = Page {
            page,
            image_type,
            path: String::new(),
            thumb_path: None,
            size: Size::default(),
        };
        if let Some(obj) = v.as_object() {
            for (k, val) in obj {
                match k.as_str() {
                    "path" => {
                        let prefix = if image_type == ImageType::Page { "i1" } else { "t1" };
                        if let Some(s) = val.as_str() {
                            out.path = format!("https://{prefix}.{host}/{s}");
                        }
                    }
                    "thumbnail" => {
                        if let Some(s) = val.as_str() {
                            out.thumb_path = Some(format!("https://t1.{host}/{s}"));
                        }
                    }
                    "width" => out.size.width = val.as_i64().unwrap_or(0) as i32,
                    "height" => out.size.height = val.as_i64().unwrap_or(0) as i32,
                    _ => {}
                }
            }
        }
        out
    }

    /// `Page(ImageType, Uri)` — plain path constructor (used by favorites
    /// DB reload); `page` = 0.
    pub fn from_path(image_type: ImageType, path: impl Into<String>) -> Page {
        Page {
            page: 0,
            image_type,
            path: path.into(),
            thumb_path: None,
            size: Size::default(),
        }
    }

    /// `getThumbnailPath` — falls back to `path` when no thumbnail exists.
    pub fn thumbnail_path(&self) -> &str {
        self.thumb_path.as_deref().unwrap_or(&self.path)
    }
}

/// Rebuild a gallery-CDN page URL from a stored path suffix
/// (`GalleryData.readPagePathNew`): covers/thumbnails use `t1`, pages `i1`.
pub fn gallery_host_url(prefix: &str, host: &str, media_id: i32, suffix: &str) -> String {
    format!("https://{prefix}.{host}/galleries/{media_id}{suffix}")
}
