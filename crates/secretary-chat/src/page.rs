//! Pagination rules of `ChatStore.hydrate` and `ChatStore.loadOlder` (`ios/Secretary/Features/Chat/ChatStore.swift`).
//!
//! The store keeps an "older" cursor in its state table (`older:mine`; an empty string means none).
use crate::merge::LOCAL_PREFIX;

/// Where the next page of older items starts: the stored older cursor when it is not empty, else the smallest
/// id among the server (non-local) items shown, else nothing.
pub fn older_cursor<'a, I>(ids: I, stored: Option<&str>) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    match stored {
        Some(s) if !s.is_empty() => Some(s.to_string()),
        _ => ids
            .into_iter()
            .filter(|id| !id.starts_with(LOCAL_PREFIX))
            .min()
            .map(str::to_string),
    }
}

/// Is there anything older to load: the cache page was full, or the server told us where to continue.
pub fn has_older(page_len: usize, page_size: usize, stored: Option<&str>) -> bool {
    page_len == page_size || stored.is_some_and(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stored_cursor_wins_when_not_empty() {
        assert_eq!(
            older_cursor(["tl_5", "tl_3"], Some("tl_1")).as_deref(),
            Some("tl_1")
        );
    }

    #[test]
    fn an_empty_or_missing_stored_cursor_falls_back_to_the_smallest_server_id() {
        for stored in [None, Some("")] {
            assert_eq!(
                older_cursor(["tl_5", "local-x", "tl_3", "a"], stored).as_deref(),
                Some("a")
            );
            assert_eq!(
                older_cursor(["tl_5", "local-a", "tl_3"], stored).as_deref(),
                Some("tl_3")
            );
        }
    }

    #[test]
    fn nothing_when_no_server_item_and_no_stored_cursor() {
        assert_eq!(older_cursor(["local-a"], None), None);
        assert_eq!(older_cursor([], Some("")), None);
    }

    #[test]
    fn has_older_when_the_page_is_full_or_a_cursor_is_stored() {
        assert!(has_older(60, 60, None));
        assert!(has_older(10, 60, Some("tl_9")));
        assert!(!has_older(10, 60, Some("")));
        assert!(!has_older(10, 60, None));
        assert!(has_older(0, 0, None), "an empty page of size zero is full");
    }
}
