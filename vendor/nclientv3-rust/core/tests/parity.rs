//! Golden parity tests: Rust output must equal the JVM output of the
//! original NClientV3 Java logic (java-goldens/JavaGolden.java →
//! java-goldens/goldens.json).

use nclientv3_core as core;
use serde_json::Value;

fn goldens() -> Value {
    let path = format!("{}/../java-goldens/goldens.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Keys in the golden maps contain raw backslashes (e.g. `hello\u0020world`
/// as literal source text); the Rust test inputs are the same strings.
fn raw(key: &str) -> String {
    key.to_string()
}

#[test]
fn parity_unescape() {
    let g = goldens();
    for (input, expected) in g["unescape"].as_object().unwrap() {
        let got = core::unescape_unicode_string(&raw(input));
        assert_eq!(got, expected.as_str().unwrap(), "unescape({input:?})");
    }
    assert_eq!(core::unescape_unicode_string(""), g["unescape_null"].as_str().unwrap());
}

#[test]
fn parity_to_query_tag() {
    let g = goldens();
    // rebuild the same tags as the harness
    let tags = [
        core::Tag::new("big breasts", 100, 42, core::TagType::Tag, core::TagStatus::Default),
        core::Tag::new("english", 999, 12227, core::TagType::Language, core::TagStatus::Avoided),
        core::Tag::new("nakadashi", 555, 777, core::TagType::Tag, core::TagStatus::Accepted),
        core::Tag::new("artist:weird \"name\"", 3, 5, core::TagType::Artist, core::TagStatus::Default),
    ];
    let mut expect = std::collections::HashMap::new();
    for t in &tags {
        expect.insert(format!("{}|{}", t.name, status_name(t.status)), t.to_query_tag());
        expect.insert(format!("{}|ACCEPTED", t.name), t.to_query_tag_with(core::TagStatus::Accepted));
    }
    for (k, v) in g["to_query_tag"].as_object().unwrap() {
        assert_eq!(&expect[k], v.as_str().unwrap(), "to_query_tag key {k}");
    }
}

fn status_name(s: core::TagStatus) -> &'static str {
    match s {
        core::TagStatus::Default => "DEFAULT",
        core::TagStatus::Avoided => "AVOIDED",
        core::TagStatus::Accepted => "ACCEPTED",
    }
}

#[test]
fn parity_to_scraped_string_and_csv_roundtrip() {
    let g = goldens();
    let tags = [
        core::Tag::new("big breasts", 100, 42, core::TagType::Tag, core::TagStatus::Default),
        core::Tag::new("english", 999, 12227, core::TagType::Language, core::TagStatus::Avoided),
        core::Tag::new("nakadashi", 555, 777, core::TagType::Tag, core::TagStatus::Accepted),
        core::Tag::new("artist:weird \"name\"", 3, 5, core::TagType::Artist, core::TagStatus::Default),
    ];
    for t in &tags {
        assert_eq!(&t.to_scraped_string(), g["to_scraped_string"][&t.name].as_str().unwrap());
        let reparsed = core::Tag::from_scraped(&t.to_scraped_string()).unwrap();
        let desc = format!(
            "{}/{}/{}/{}",
            reparsed.name,
            reparsed.count,
            reparsed.id,
            type_name(reparsed.type_)
        );
        assert_eq!(&desc, g["csv_roundtrip"][&t.name].as_str().unwrap(), "csv {t:?}");
    }
}

fn type_name(t: core::TagType) -> &'static str {
    match t {
        core::TagType::Unknown => "UNKNOWN",
        core::TagType::Parody => "PARODY",
        core::TagType::Character => "CHARACTER",
        core::TagType::Tag => "TAG",
        core::TagType::Artist => "ARTIST",
        core::TagType::Group => "GROUP",
        core::TagType::Language => "LANGUAGE",
        core::TagType::Category => "CATEGORY",
    }
}

#[test]
fn parity_ranges() {
    let g = goldens();
    let mut r = core::Ranges::new();
    assert_eq!(format!("{}|isDefault={}", r.to_query(), r.is_default()), g["ranges"]["default"].as_str().unwrap());
    let mut r = core::Ranges::new();
    r.from_page = 10;
    r.to_page = 10;
    assert_eq!(format!("{}|isDefault={}", r.to_query(), r.is_default()), g["ranges"]["pages_eq"].as_str().unwrap());
    let mut r = core::Ranges::new();
    r.from_page = 5;
    r.to_page = 40;
    assert_eq!(r.to_query(), g["ranges"]["pages_range"].as_str().unwrap());
    let mut r = core::Ranges::new();
    r.from_date = Some((3, core::TimeUnit::Day));
    assert_eq!(r.to_query(), g["ranges"]["uploaded_from"].as_str().unwrap());
    let r = core::Ranges {
        to_date: Some((2, core::TimeUnit::Week)),
        ..core::Ranges::new()
    };
    assert_eq!(r.to_query(), g["ranges"]["uploaded_to"].as_str().unwrap());
    let mut r = core::Ranges::new();
    r.from_date = Some((7, core::TimeUnit::Month));
    r.to_date = Some((7, core::TimeUnit::Month));
    assert_eq!(r.to_query(), g["ranges"]["uploaded_eq"].as_str().unwrap());
    let mut r = core::Ranges::new();
    r.from_page = 1;
    r.to_page = 200;
    r.from_date = Some((1, core::TimeUnit::Year));
    assert_eq!(r.to_query(), g["ranges"]["mixed"].as_str().unwrap());
    let mut r = core::Ranges::new();
    r.from_page = 2;
    r.to_page = 4;
    r.to_date = Some((1, core::TimeUnit::Hour));
    assert_eq!(r.to_query(), g["ranges"]["mixed2"].as_str().unwrap());
}

#[test]
fn parity_sort_find() {
    let g = goldens();
    let cases: Vec<Option<&str>> = vec![
        None,
        Some("popular"),
        Some("popular-week"),
        Some("xpopular-todayx"),
        Some("popular-month"),
        Some("recent"),
        Some("popular-we"),
        Some(""),
    ];
    for c in cases {
        let key = c.map(|s| s.to_string()).unwrap_or_else(|| "null".to_string());
        let got = match core::SortType::find_from_addition(c) {
            core::SortType::RecentAllTime => "RECENT_ALL_TIME",
            core::SortType::PopularAllTime => "POPULAR_ALL_TIME",
            core::SortType::PopularWeekly => "POPULAR_WEEKLY",
            core::SortType::PopularDaily => "POPULAR_DAILY",
            core::SortType::PopularMonth => "POPULAR_MONTH",
        };
        assert_eq!(got, g["sort_find"][&key].as_str().unwrap(), "find({c:?})");
    }
}

#[test]
fn parity_local_sort() {
    let g = goldens();
    let kinds = [
        (core::LocalSortKind::Title, "TITLE"),
        (core::LocalSortKind::Date, "DATE"),
        (core::LocalSortKind::PageCount, "PAGE_COUNT"),
        (core::LocalSortKind::Artist, "ARTIST"),
        (core::LocalSortKind::Group, "GROUP"),
        (core::LocalSortKind::Random, "RANDOM"),
    ];
    for (k, name) in kinds {
        for desc in [false, true] {
            let st = core::LocalSortType { kind: k, descending: desc };
            let hash = st.hash();
            let got = format!("hash={hash};type={name};desc={desc}");
            let key = format!("{name}/{desc}");
            assert_eq!(&got, g["local_sort"][&key].as_str().unwrap(), "key {key}");
        }
    }
    // raw out-of-range hashes
    let d130 = core::LocalSortType::from_hash(130);
    let got = format!(
        "type={};desc={}",
        type_name_local(d130.kind),
        d130.descending
    );
    assert_eq!(got, g["local_sort"]["raw130"].as_str().unwrap());
    let dm = core::LocalSortType::from_hash(-128);
    let got = format!(
        "type={};desc={}",
        type_name_local(dm.kind),
        dm.descending
    );
    assert_eq!(got, g["local_sort"]["raw-128"].as_str().unwrap());
}

fn type_name_local(k: core::LocalSortKind) -> &'static str {
    match k {
        core::LocalSortKind::Title => "TITLE",
        core::LocalSortKind::Date => "DATE",
        core::LocalSortKind::PageCount => "PAGE_COUNT",
        core::LocalSortKind::Artist => "ARTIST",
        core::LocalSortKind::Group => "GROUP",
        core::LocalSortKind::Random => "RANDOM",
    }
}

#[test]
fn parity_path_title() {
    let g = goldens();
    let titles = [
        "[Artist] My (Great) Title / Vol. 2?",
        "slashes/and|pipes*star\"quote'apos:colon<lt>gt",
        "  double   spaces  ",
        "tab\tchar",
        "",
    ];
    for t in titles {
        assert_eq!(core::path_title(Some(t)), g["path_title"][t].as_str().unwrap(), "{t:?}");
    }
    assert_eq!(core::path_title_with_default(None, "fallback"), g["path_title"]["null-default-"].as_str().unwrap());
}

#[test]
fn parity_load_language() {
    let g = goldens();
    let mk = |id: i32| core::Tag::new("x", 0, id, core::TagType::Language, core::TagStatus::Default);
    let list = |tags: Vec<core::Tag>| {
        let mut l = core::TagList::new();
        l.add_tags(tags);
        l
    };
    let lang_name = |l: core::Language| match l {
        core::Language::English => "ENGLISH",
        core::Language::Chinese => "CHINESE",
        core::Language::Japanese => "JAPANESE",
        core::Language::All => "ALL",
        core::Language::Unknown => "UNKNOWN",
    };
    let got = lang_name(core::gallery::load_language(&list(vec![mk(12227)])));
    assert_eq!(got, g["load_language"]["english"].as_str().unwrap());
    let got = lang_name(core::gallery::load_language(&list(vec![mk(6346)])));
    assert_eq!(got, g["load_language"]["japanese"].as_str().unwrap());
    let got = lang_name(core::gallery::load_language(&list(vec![mk(29963)])));
    assert_eq!(got, g["load_language"]["chinese"].as_str().unwrap());
    let got = lang_name(core::gallery::load_language(&list(vec![mk(1)])));
    assert_eq!(got, g["load_language"]["other"].as_str().unwrap());
    let mut l = core::TagList::new();
    l.add_tags(vec![
        core::Tag::new("artist", 0, 9, core::TagType::Artist, core::TagStatus::Default),
        mk(29963),
        mk(12227),
    ]);
    let got = lang_name(core::gallery::load_language(&l));
    assert_eq!(got, g["load_language"]["mixed"].as_str().unwrap());
}

#[test]
fn parity_create_url_matrix() {
    let g = goldens();
    let u = core::inspector::create_url;
    let host = core::DEFAULT_HOST;
    let search_tags = vec![
        core::Tag::new("nakadashi", 555, 777, core::TagType::Tag, core::TagStatus::Accepted),
        core::Tag::new("vanilla", 200, 300, core::TagType::Tag, core::TagStatus::Default),
        core::Tag::new("english", 999, 12227, core::TagType::Language, core::TagStatus::Avoided),
    ];
    assert_eq!(
        u(core::ApiRequestType::ByAll, None, 0, 0, &[], None, Some(core::SortType::RecentAllTime), host),
        g["urls"]["byall"].as_str().unwrap()
    );
    assert_eq!(
        u(core::ApiRequestType::ByAll, None, 3, 0, &[], None, Some(core::SortType::RecentAllTime), host),
        g["urls"]["byall_p3"].as_str().unwrap()
    );
    assert_eq!(u(core::ApiRequestType::Random, None, 0, 0, &[], None, None, host), g["urls"]["random"].as_str().unwrap());
    assert_eq!(
        u(core::ApiRequestType::RandomFavorite, None, 0, 0, &[], None, None, host),
        g["urls"]["random_fav"].as_str().unwrap()
    );
    assert_eq!(
        u(core::ApiRequestType::BySingle, None, 0, 12345, &[], None, None, host),
        g["urls"]["bysingle"].as_str().unwrap()
    );
    assert_eq!(u(core::ApiRequestType::Favorite, None, 1, 0, &[], None, None, host), g["urls"]["favorite"].as_str().unwrap());
    assert_eq!(
        u(core::ApiRequestType::Favorite, Some("big hero"), 2, 0, &[], None, None, host),
        g["urls"]["favorite_q"].as_str().unwrap()
    );
    assert_eq!(
        u(
            core::ApiRequestType::ByTag,
            Some(""),
            1,
            0,
            &[core::Tag::new("nakadashi", 1, 2, core::TagType::Tag, core::TagStatus::Accepted)],
            None,
            Some(core::SortType::PopularAllTime),
            host
        ),
        g["urls"]["bytag"].as_str().unwrap()
    );
    assert_eq!(
        u(core::ApiRequestType::BySearch, Some("gundam"), 2, 0, &search_tags, None, Some(core::SortType::PopularWeekly), host),
        g["urls"]["bysearch"].as_str().unwrap()
    );
    let mut rr = core::Ranges::new();
    rr.from_page = 1;
    rr.to_page = 30;
    rr.from_date = Some((1, core::TimeUnit::Month));
    assert_eq!(
        u(core::ApiRequestType::BySearch, Some(""), 1, 0, &search_tags, Some(&rr), Some(core::SortType::PopularMonth), host),
        g["urls"]["bysearch_ranges"].as_str().unwrap()
    );
    // the harness builds the input from runtime backslashes: literal `\u4e2d\u6587 + title`
    let unicode_query = format!("\\u4e2d\\u6587 + title");
    assert_eq!(
        u(
            core::ApiRequestType::BySearch,
            Some(&unicode_query),
            1,
            0,
            &[core::Tag::new("artist:weird \"name\"", 3, 5, core::TagType::Artist, core::TagStatus::Default)],
            None,
            Some(core::SortType::RecentAllTime),
            host
        ),
        g["urls"]["bysearch_unicode"].as_str().unwrap()
    );
    assert_eq!(
        u(
            core::ApiRequestType::BySearch,
            Some(""),
            1,
            0,
            &[
                core::Tag::new("dup", 1, 2, core::TagType::Tag, core::TagStatus::Accepted),
                core::Tag::new("dup", 1, 2, core::TagType::Tag, core::TagStatus::Default),
            ],
            None,
            None,
            host
        ),
        g["urls"]["bysearch_dup"].as_str().unwrap()
    );
}

#[test]
fn parity_bookmark_and_search_title() {
    let g = goldens();
    let bysearch = g["urls"]["bysearch"].as_str().unwrap();
    let byall_p3 = g["urls"]["byall_p3"].as_str().unwrap();
    assert_eq!(
        core::inspector::get_bookmark_url(byall_p3, 0),
        g["bookmark"]["page1"].as_str().unwrap()
    );
    assert_eq!(
        core::inspector::get_bookmark_url(bysearch, 2),
        g["bookmark"]["page2"].as_str().unwrap()
    );
    assert_eq!(
        core::inspector::get_search_title("", bysearch, core::DEFAULT_HOST),
        g["bookmark"]["search_title"].as_str().unwrap()
    );
    assert_eq!(
        core::inspector::get_search_title("gundam", bysearch, core::DEFAULT_HOST),
        g["bookmark"]["search_title_q"].as_str().unwrap()
    );
}

#[test]
fn parity_url_encode() {
    let g = goldens();
    let bs = '\\';
    let inputs: Vec<(String, &str)> = vec![
        ("hello world".into(), "hello world"),
        ("a+b".into(), "a+b"),
        (format!("{bs}u4e2d{bs}u6587"), "key2"),
        ("100%".into(), "100%"),
        ("tag/name".into(), "tag/name"),
        ("a*b-c_d.e".into(), "a*b-c_d.e"),
        ("quote\"'?".into(), "quote\"'?"),
    ];
    let map = g["url_encode"].as_object().unwrap();
    for (input, _) in &inputs {
        assert_eq!(
            core::java_url_encode(input),
            map[input.as_str()].as_str().unwrap(),
            "encode({input:?})"
        );
    }
}

#[test]
fn parity_decisions() {
    let g = goldens();
    let one_accepted = vec![core::Tag::new("nakadashi", 1, 2, core::TagType::Tag, core::TagStatus::Accepted)];
    let one_avoided = vec![core::Tag::new("english", 1, 12227, core::TagType::Language, core::TagStatus::Avoided)];
    let mut two_tags = one_accepted.clone();
    two_tags.extend(one_avoided.clone());

    let fmt = |(t, q): (core::ApiRequestType, String)| {
        let tn = match t {
            core::ApiRequestType::ByAll => "BYALL",
            core::ApiRequestType::ByTag => "BYTAG",
            core::ApiRequestType::BySearch => "BYSEARCH",
            core::ApiRequestType::BySingle => "BYSINGLE",
            core::ApiRequestType::Related => "RELATED",
            core::ApiRequestType::Favorite => "FAVORITE",
            core::ApiRequestType::Random => "RANDOM",
            core::ApiRequestType::RandomFavorite => "RANDOM_FAVORITE",
        };
        format!("{tn}|{q}")
    };
    let d = core::inspector::decide_request_type;
    assert_eq!(fmt(d("", &[], true, core::SortType::RecentAllTime)), g["decisions"]["empty_no_tags_recent"].as_str().unwrap());
    assert_eq!(fmt(d("", &[], true, core::SortType::PopularAllTime)), g["decisions"]["empty_no_tags_popular"].as_str().unwrap());
    assert_eq!(fmt(d("", &one_accepted, true, core::SortType::RecentAllTime)), g["decisions"]["empty_one_accepted"].as_str().unwrap());
    assert_eq!(fmt(d("", &one_avoided, true, core::SortType::RecentAllTime)), g["decisions"]["empty_one_avoided"].as_str().unwrap());
    assert_eq!(fmt(d("", &two_tags, true, core::SortType::RecentAllTime)), g["decisions"]["empty_two_tags"].as_str().unwrap());
    assert_eq!(fmt(d("gundam", &[], true, core::SortType::RecentAllTime)), g["decisions"]["query_no_tags"].as_str().unwrap());
    assert_eq!(fmt(d("", &one_accepted, false, core::SortType::RecentAllTime)), g["decisions"]["empty_ranges_custom"].as_str().unwrap());
}

#[test]
fn parity_json_parsing_end_to_end() {
    // Behavioral vectors derived from the Java JsonReader switch logic
    // (GalleryData / Tag / Page / SimpleGallery.fromV2ListItem / doSingleV2).
    let detail = serde_json::json!({
        "id": 1770003,
        "media_id": "913216",
        "upload_date": 1700000000i64,
        "num_favorites": 42,
        "num_pages": 2,
        "title": {"japanese": "\\u4e2d\\u6587", "english": "Eng Title", "pretty": null},
        "tags": [
            {"id": 12227, "type": "language", "name": "english", "count": 999, "slug": "english"},
            {"id": 42, "type": "tag", "name": "big breasts", "count": 100},
            {"id": 777, "type": "artist", "name": "someone", "count": 50}
        ],
        "cover": {"path": "galleries/913216/cover.jpg", "width": 300, "height": 400},
        "thumbnail": {"path": "galleries/913216/thumb.jpg", "w": 1},
        "pages": [
            {"path": "galleries/913216/1.jpg", "width": 1280, "height": 1810},
            {"path": "galleries/913216/2.jpg", "width": 1000, "height": 1400}
        ],
        "unknown_key": {"nested": true}
    });
    let host = core::DEFAULT_HOST;
    let d = core::GalleryData::parse_json(&detail, host).unwrap();
    assert!(d.is_valid());
    assert_eq!(d.id, 1770003);
    assert_eq!(d.media_id, 913216);
    assert_eq!(d.upload_date_millis, 1_700_000_000_000);
    assert_eq!(d.favorite_count, 42);
    assert_eq!(d.page_count, 2);
    // titles: unescape applied, JSON null → ""
    assert_eq!(d.get_title(core::TitleType::Japanese), "中文");
    assert_eq!(d.get_title(core::TitleType::English), "Eng Title");
    assert_eq!(d.get_title(core::TitleType::Pretty), "");
    // tags sorted by count desc within buckets
    let tag_tags = d.tags.retrieve_for_type(core::TagType::Tag);
    assert_eq!(tag_tags.len(), 1);
    assert_eq!(tag_tags[0].name, "big breasts");
    assert_eq!(d.tags.retrieve_for_type(core::TagType::Artist)[0].id, 777);
    // pages got sequential indices
    assert_eq!(d.pages[0].page, 0);
    assert_eq!(d.pages[1].page, 1);
    assert_eq!(d.pages[0].path, "https://i1.nhentai.net/galleries/913216/1.jpg");
    assert_eq!(d.cover.path, "https://t1.nhentai.net/galleries/913216/cover.jpg");
    assert_eq!(d.thumbnail.path, "https://t1.nhentai.net/galleries/913216/thumb.jpg");
    // language derived from special ids
    assert_eq!(core::gallery::load_language(&d.tags), core::Language::English);

    // error response → invalid
    let err: serde_json::Value = serde_json::json!({"error": "not found"});
    let d2 = core::GalleryData::parse_json(&err, host).unwrap();
    assert!(!d2.is_valid());

    // RANDOM short form
    let provider = core::simple_gallery::EmptyTagProvider;
    match core::inspector::parse_single_response("{\"id\": 12345}", host, &provider).unwrap() {
        core::inspector::SingleParseOutcome::NeedsDetailFetch { gallery_id } => {
            assert_eq!(gallery_id, 12345);
            assert_eq!(
                core::inspector::detail_url(12345, host),
                "https://nhentai.net/api/v2/galleries/12345?include=related,favorite"
            );
        }
        other => panic!("expected NeedsDetailFetch, got {other:?}"),
    }

    // related list + favorite flag on a full detail
    let mut with_related = detail.clone();
    with_related["is_favorited"] = serde_json::json!(true);
    with_related["related"] = serde_json::json!([
        {"id": 5, "media_id": "55", "thumbnail": "x.jpg", "english_title": "Rel", "japanese_title": "日本語", "tag_ids": [12227], "num_pages": 3}
    ]);
    match core::inspector::parse_single_response(&with_related.to_string(), host, &provider).unwrap() {
        core::inspector::SingleParseOutcome::Gallery(gp) => {
            assert!(gp.is_favorite);
            assert_eq!(gp.related.len(), 1);
            assert_eq!(gp.related[0].title, "Rel");
            assert_eq!(gp.related[0].thumbnail, "https://t1.nhentai.net/x.jpg");
            assert_eq!(gp.language, core::Language::English);
        }
        other => panic!("expected Gallery, got {other:?}"),
    }

    // search response: num_pages fallback + exact tag filter
    let search = serde_json::json!({
        "result": [
            {"id": 1, "media_id": "11", "thumbnail": "a.jpg", "english_title": "A", "japanese_title": "", "tag_ids": [], "num_pages": 1},
            {"id": 2, "media_id": "22", "thumbnail": "b.jpg", "english_title": "B", "japanese_title": "", "tag_ids": [], "num_pages": 1}
        ],
        "num_pages": 7
    });
    let (gals, pages) = core::inspector::parse_search_response(
        &search.to_string(),
        host,
        false,
        &[],
        3,
        &provider,
    )
    .unwrap();
    assert_eq!(gals.len(), 2);
    assert_eq!(pages, 7);
    let (gals, pages) = core::inspector::parse_search_response(
        "{\"result\": []}",
        host,
        false,
        &[],
        3,
        &provider,
    )
    .unwrap();
    assert_eq!(pages, 3, "optInt fallback max(1, page)");
    assert!(gals.is_empty());

    // missing result → InvalidResponse (Java throws)
    assert!(matches!(
        core::inspector::parse_search_response("{}", host, false, &[], 1, &provider),
        Err(core::inspector::InspectorError::InvalidResponse)
    ));

    // comments
    let comments = core::comments::parse_comments(
        "[{\"id\": 9, \"post_date\": 1700000000, \"body\": \"nice\", \"poster\": {\"id\": 3, \"post_date\": \"poster-name\", \"avatar_url\": \"av.png\"}}]",
    );
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].id, 9);
    assert_eq!(comments[0].post_date_millis, Some(1_700_000_000_000));
    // preserved upstream quirk: username read from "post_date"
    assert_eq!(comments[0].username(), Some("poster-name"));
    assert_eq!(
        comments[0].poster.as_ref().unwrap().avatar_url_full(host),
        "https://i.nhentai.net/av.png"
    );
    assert_eq!(core::comments::comments_url(42, host), "https://nhentai.net/api/v2/galleries/42/comments");

    // create_page_path / parse_page_path roundtrip (new format)
    let path = d.create_page_path();
    let (back, state) =
        core::GalleryData::parse_page_path(&path, host, d.media_id, d.id).unwrap();
    assert_eq!(state, core::gallery_data::PagePathState::Loaded);
    assert_eq!(back.cover.path, d.cover.path);
    assert_eq!(back.thumbnail.path, d.thumbnail.path);
    assert_eq!(back.pages.len(), d.pages.len());
    assert_eq!(back.pages[1].path, d.pages[1].path);

    // fake data
    let fake = core::GalleryData::fake_data();
    assert!(!fake.is_valid());
    assert_eq!(fake.id, -1);
    assert_eq!(fake.favorite_count, -1);
    assert_eq!(fake.page_count, -1);

    // full Gallery derive: min/max sizes
    let gal = core::Gallery::parse(&detail, Vec::new(), false, host).unwrap();
    assert_eq!(gal.max_size, core::Size::new(1280, 1810));
    assert_eq!(gal.min_size, core::Size::new(1000, 1400));
    assert!(!gal.is_online_favorite());

    // title fallback chain
    assert_eq!(gal.get_title_with_pref(core::TitleType::English), "Eng Title");
    let mut no_titles = detail.clone();
    no_titles["title"] = serde_json::json!({"japanese": "", "english": "", "pretty": ""});
    let gal2 = core::Gallery::parse(&no_titles, Vec::new(), false, host).unwrap();
    assert_eq!(gal2.get_title_with_pref(core::TitleType::Japanese), "Unnamed");
}
