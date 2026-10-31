//! Port of `api/InspectorV3.java` — the portable core: request-type
//! decision, URL construction, and response parsing. The Android class also
//! subclasses `Thread` and owns the HTTP client; here the caller performs IO
//! and feeds JSON into the parsers.

use crate::enums::{ApiRequestType, SortType, TagStatus};
use crate::gallery_data::GalleryData;
use crate::page::Size;
use crate::ranges::Ranges;
use crate::simple_gallery::SimpleGallery;
use crate::tag::Tag;
use crate::util::{base_url, java_url_encode};
use serde::{Deserialize, Serialize};

/// `InspectorV3.getTag()` over an ordered tag list: the first non-LANGUAGE
/// tag, else the last tag, else `None`.
pub fn inspector_get_tag<'a>(tags: &'a [Tag]) -> Option<&'a Tag> {
    let mut last = None;
    for t in tags {
        if t.type_ != crate::enums::TagType::Language {
            return Some(t);
        }
        last = Some(t);
    }
    last
}

/// `InspectorV3.tryByAllPopular` — mutates request type + query like Java.
fn try_by_all_popular(sort_type: SortType, request_type: &mut ApiRequestType, query: &mut String) {
    if sort_type != SortType::RecentAllTime {
        *request_type = ApiRequestType::BySearch;
        *query = "-nclientv3".to_string();
    }
}

/// `InspectorV3.searchInspector`'s request-type decision block.
/// `tags` = user tags + default/language/blacklisted tags already resolved
/// (the Java code pulls them from the DB). `ranges_apply` is true when the
/// ranges are null-or-default (Java: `ranges == null || ranges.isDefault()`).
/// Returns `(requestType, effectiveQuery)`.
pub fn decide_request_type(
    query: &str,
    tags: &[Tag],
    ranges_apply: bool,
    sort_type: SortType,
) -> (ApiRequestType, String) {
    let mut effective = query.to_string();
    let mut request_type;
    if query.is_empty() && ranges_apply {
        match tags.len() {
            0 => {
                request_type = ApiRequestType::ByAll;
                try_by_all_popular(sort_type, &mut request_type, &mut effective);
            }
            1 => {
                // BYTAG; else by search for the negative tag
                if tags[0].status != TagStatus::Avoided {
                    request_type = ApiRequestType::ByTag;
                } else {
                    request_type = ApiRequestType::BySearch;
                }
            }
            _ => request_type = ApiRequestType::BySearch,
        }
    } else {
        request_type = ApiRequestType::BySearch;
    }
    (request_type, effective)
}

/// `InspectorV3.createUrl` — byte-for-byte port. Steps:
/// 1. URL-encode the query with `java.net.URLEncoder` semantics.
/// 2. Per-type path building (see Java branches).
/// 3. For search/tag: append each tag (URL-encoded `toQueryTag()`) after a
///    `+`, skipping tags whose ACCEPTED form is already contained in the
///    builder; then optional `+ranges`, `&page=N`, `&sort=...`.
/// 4. Replace every remaining space with `+` across the WHOLE URL.
pub fn create_url(
    request_type: ApiRequestType,
    query: Option<&str>,
    page: i32,
    id: i32,
    tags: &[Tag],
    ranges: Option<&Ranges>,
    sort_type: Option<SortType>,
    host: &str,
) -> String {
    let encoded_query: Option<String> = query.map(java_url_encode);
    let mut builder = base_url(host);
    builder.push_str("api/v2/");
    match request_type {
        ApiRequestType::ByAll => {
            builder.push_str("galleries?page=");
            builder.push_str(&page.to_string());
        }
        ApiRequestType::Random => builder.push_str("galleries/random"),
        ApiRequestType::RandomFavorite => builder.push_str("favorites/random"),
        ApiRequestType::BySingle => {
            builder.push_str("galleries/");
            builder.push_str(&id.to_string());
            builder.push_str("?include=related,favorite");
        }
        ApiRequestType::Favorite => {
            builder.push_str("favorites?page=");
            builder.push_str(&page.to_string());
            if let Some(q) = &encoded_query {
                if !q.is_empty() {
                    builder.push_str("&q=");
                    builder.push_str(q);
                }
            }
        }
        ApiRequestType::BySearch | ApiRequestType::ByTag => {
            builder.push_str("search?query=");
            builder.push_str(encoded_query.as_deref().unwrap_or(""));
            for tt in tags {
                if builder.contains(&tt.to_query_tag_with(TagStatus::Accepted)) {
                    continue;
                }
                builder.push('+');
                builder.push_str(&java_url_encode(&tt.to_query_tag()));
            }
            if let Some(r) = ranges {
                builder.push('+');
                builder.push_str(&r.to_query());
            }
            builder.push_str("&page=");
            builder.push_str(&page.to_string());
            if let Some(st) = sort_type {
                if let Some(add) = st.url_addition() {
                    builder.push_str("&sort=");
                    builder.push_str(add);
                }
            }
        }
        // RELATED never reaches createUrl in the Java code (no branch); keep
        // the URL unchanged after the api/v2/ prefix, like Java would.
        ApiRequestType::Related => {}
    }
    builder.replace(' ', "+")
}

/// `InspectorV3.getBookmarkURL`
pub fn get_bookmark_url(url: &str, page: i32) -> String {
    if page < 2 {
        url.to_string()
    } else {
        match url.rfind('=') {
            Some(i) => url[..i + 1].to_string(),
            None => url.to_string(), // Java would throw StringIndexOutOfBounds
        }
    }
}

/// `InspectorV3.getSearchTitle`
pub fn get_search_title(query: &str, url: &str, host: &str) -> String {
    if !query.is_empty() {
        return query.to_string();
    }
    url.replace(&format!("{}api/v2/search?query=", base_url(host)), "")
        .replace('+', " ")
}

/// Errors from response parsing (`InspectorV3.InvalidResponseException` and
/// the JSONException path share this in the port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectorError {
    /// no `result` array / malformed JSON
    InvalidResponse,
}

/// `InspectorV3.doSearchV2` — parse a search/list response.
/// `tag_filter`: when `exact_tag_match` is set (Java `Global.isExactTagMatch()`),
/// galleries missing any of `filter_tags` are dropped
/// (`filterDocumentTags`).
/// Returns `(galleries, pageCount)`; missing `num_pages` falls back to
/// `max(1, page)` like Java `optInt("num_pages", Math.max(1, page))`.
pub fn parse_search_response(
    body: &str,
    host: &str,
    exact_tag_match: bool,
    filter_tags: &[Tag],
    page: i32,
    tag_provider: &dyn crate::simple_gallery::TagProvider,
) -> Result<(Vec<SimpleGallery>, i32), InspectorError> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|_| InspectorError::InvalidResponse)?;
    let results = json
        .get("result")
        .and_then(|v| v.as_array())
        .ok_or(InspectorError::InvalidResponse)?;
    let mut galleries = Vec::with_capacity(results.len());
    for item in results {
        let g = SimpleGallery::from_v2_list_item(item, host, tag_provider)
            .ok_or(InspectorError::InvalidResponse)?;
        galleries.push(g);
    }
    let page_count = json
        .get("num_pages")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32)
        .unwrap_or(1.max(page));
    if exact_tag_match {
        galleries.retain(|g| g.has_tags(filter_tags));
    }
    Ok((galleries, page_count))
}

/// Output of `InspectorV3.doSingleV2`.
#[derive(Debug, Clone)]
pub enum SingleParseOutcome {
    /// response contained `"error"` → empty gallery list (Java returns early)
    Error,
    /// RANDOM responses are `{"id": N}` — Java fetches the detail URL next:
    /// `<base>api/v2/galleries/<id>?include=related,favorite`
    NeedsDetailFetch { gallery_id: i32 },
    /// full single-gallery parse (detail response)
    Gallery(Box<GalleryParse>),
}

/// Parsed single gallery with its related list and favorite flag
/// (Java `new Gallery(context, json, relatedList, isFavorite)`).
#[derive(Debug, Clone)]
pub struct GalleryParse {
    pub data: GalleryData,
    pub related: Vec<SimpleGallery>,
    pub is_favorite: bool,
    pub language: crate::enums::Language,
    pub max_size: Size,
    pub min_size: Size,
}

/// `InspectorV3.doSingleV2` minus the network fetch. Feed the JSON of the
/// endpoint response; when the RANDOM short form is detected the port tells
/// the caller which detail URL to fetch instead of doing it inline.
pub fn parse_single_response(
    body: &str,
    host: &str,
    tag_provider: &dyn crate::simple_gallery::TagProvider,
) -> Result<SingleParseOutcome, InspectorError> {
    let v2: serde_json::Value =
        serde_json::from_str(body).map_err(|_| InspectorError::InvalidResponse)?;
    if v2.get("error").is_some() {
        return Ok(SingleParseOutcome::Error);
    }

    // Random endpoint returns only {"id": N} — caller must fetch full detail.
    if v2.get("title").is_none() && v2.get("id").is_some() {
        let gallery_id = v2
            .get("id")
            .and_then(|v| v.as_i64())
            .ok_or(InspectorError::InvalidResponse)? as i32;
        return Ok(SingleParseOutcome::NeedsDetailFetch { gallery_id });
    }

    let mut related = Vec::new();
    if let Some(arr) = v2.get("related").and_then(|v| v.as_array()) {
        for item in arr {
            related.push(
                SimpleGallery::from_v2_list_item(item, host, tag_provider)
                    .ok_or(InspectorError::InvalidResponse)?,
            );
        }
    }
    let is_favorite = v2
        .get("is_favorited")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // Java constructs `new Gallery(context, v2.toString(), ...)` which re-parses
    // the same JSON; the port parses once.
    let data = GalleryData::parse_json(&v2, host).ok_or(InspectorError::InvalidResponse)?;
    let language = crate::gallery::load_language(&data.tags);
    let (max_size, min_size) = crate::gallery_data::calculate_sizes(&data.pages);
    Ok(SingleParseOutcome::Gallery(Box::new(GalleryParse {
        data,
        related,
        is_favorite,
        language,
        max_size,
        min_size,
    })))
}

/// Detail URL for a RANDOM short-form response (`doSingleV2` + `Utility`).
pub fn detail_url(gallery_id: i32, host: &str) -> String {
    format!(
        "{}api/v2/galleries/{gallery_id}?include=related,favorite",
        base_url(host)
    )
}

/// Sort addition lookup used by callers that rebuild URLs
/// (`InspectorV3.setSortType` re-runs `createUrl`).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct InspectorParams {
    pub sort_type: Option<SortType>,
    pub page: i32,
    pub id: i32,
}
