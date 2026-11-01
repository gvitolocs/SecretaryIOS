//! Layout determinism, anchor stability, justification, dirty regions and an emulated reading session.
//! Uses the OFL Liberation Serif fixture (see fixtures/LICENSE-LiberationSerif-OFL.txt).
use ink_epub::text::{from_markdown, from_txt};
use ink_epub::Book;
use ink_kindle_hal::input::TouchEvent;
use ink_kindle_hal::sim::{ScriptedInput, SimDisplay};
use ink_kindle_hal::{GestureConfig, GrayImage, Input, Point, RefreshPolicy, PW3_SIZE};
use ink_render::layout::{content_height, layout_chapter, LineContent};
use ink_render::render::render_page;
use ink_render::{paginate, Anchor, Fonts, Reader, Style, FONT_SIZES};

const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/LiberationSerif-Regular.ttf"
);

fn fonts() -> Fonts {
    Fonts::load(std::path::Path::new(FONT)).expect("fixture font")
}

/// Deterministic prose with chapters, headings, emphasis, lists, quotes, a long word and an image.
fn sample_book(paragraphs: usize) -> Book {
    let words = [
        "lantern",
        "harbour",
        "quietly",
        "the",
        "of",
        "and",
        "a",
        "whispering",
        "meadow",
        "river",
        "extraordinarily",
        "she",
        "remembered",
        "window",
        "evening",
        "in",
        "to",
        "was",
        "old",
        "carriage",
        "silver",
        "mountain",
        "letters",
        "beneath",
        "candle",
        "it",
    ];
    let mut s = String::new();
    let mut seed = 7u64;
    let mut rnd = |n: usize| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 33) as usize % n
    };
    for p in 0..paragraphs {
        if p % 25 == 0 {
            s.push_str(&format!("# Chapter {}\n\n", p / 25 + 1));
        } else if p % 9 == 0 {
            s.push_str("## A section heading that is fairly long\n\n");
        }
        if p % 17 == 5 {
            s.push_str("- a list item\n- another *list* item with more words in it than fit on one line of the page\n\n");
        }
        if p % 23 == 11 {
            s.push_str("> A quotation, set apart from the text, which runs over a couple of lines at least.\n\n");
        }
        if p == 40 {
            s.push_str("![A drawing of the harbour](img/h.png)\n\n");
            s.push_str(&"Pneumonoultramicroscopic".repeat(6));
            s.push_str("\n\n");
        }
        let n = 30 + rnd(60);
        for i in 0..n {
            let w = words[rnd(words.len())];
            let w = match rnd(14) {
                0 => format!("*{w}*"),
                1 => format!("**{w}**"),
                _ => w.to_owned(),
            };
            s.push_str(&w);
            s.push_str(if i + 1 == n { ".\n\n" } else { " " });
        }
    }
    from_markdown(s.as_bytes(), "Sample")
}

fn hash(img: &GrayImage) -> u64 {
    img.data.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

#[test]
fn layout_and_rendering_are_deterministic() {
    let book = sample_book(120);
    let style = Style::default();
    let mut f1 = fonts();
    let a = paginate(&mut f1, &style, PW3_SIZE, &book);
    let b = paginate(&mut f1, &style, PW3_SIZE, &book); // warm caches
    let mut f2 = fonts();
    let c = paginate(&mut f2, &style, PW3_SIZE, &book); // fresh instance
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert!(a.pages.len() > 10, "{} pages", a.pages.len());
    for page in [0, 3, a.pages.len() / 2, a.pages.len() - 1] {
        let mut i1 = GrayImage::new(PW3_SIZE, 0);
        let mut i2 = GrayImage::new(PW3_SIZE, 0);
        render_page(&mut i1, &mut f1, &book, &a, page);
        render_page(&mut i2, &mut f2, &book, &c, page);
        assert_eq!(hash(&i1), hash(&i2), "page {page}");
        assert!(i1.data.iter().any(|&p| p < 128), "page {page} has ink");
    }
}

#[test]
fn pages_tile_each_chapter_and_fit_the_panel() {
    let book = sample_book(150);
    let style = Style::default();
    let mut f = fonts();
    let pag = paginate(&mut f, &style, PW3_SIZE, &book);
    let max_h = content_height(PW3_SIZE, &style);
    for (i, p) in pag.pages.iter().enumerate() {
        let ch = &book.chapters[p.chapter as usize];
        if i == 0 || pag.pages[i - 1].chapter != p.chapter {
            assert_eq!(p.start, 0);
        } else {
            assert_eq!(pag.pages[i - 1].end, p.start, "contiguous");
        }
        if i + 1 == pag.pages.len() || pag.pages[i + 1].chapter != p.chapter {
            assert_eq!(p.end, ch.char_len());
        }
        for pl in &p.lines {
            assert!(
                pl.y + pl.line.height <= max_h || p.lines.len() == 1,
                "line inside page"
            );
        }
        // a heading never ends a page when more text follows in the chapter
        if let Some(last) = p.lines.last() {
            if last.line.keep_with_next && p.end < ch.char_len() {
                let chapter_continues =
                    pag.pages.get(i + 1).is_some_and(|n| n.chapter == p.chapter);
                let only_headings = p.lines.iter().all(|l| l.line.keep_with_next);
                assert!(
                    !chapter_continues || only_headings,
                    "orphaned heading on page {i}"
                );
            }
        }
    }
}

#[test]
fn justified_lines_fill_the_measure_and_nothing_overflows() {
    let book = sample_book(60);
    let style = Style::default();
    let mut f = fonts();
    let measure = (PW3_SIZE.w - style.margins.left - style.margins.right) as f32;
    let mut justified = 0;
    for ch in &book.chapters {
        let lines = layout_chapter(&mut f, &style, measure, ch);
        for l in &lines {
            let LineContent::Text(runs) = &l.content else {
                continue;
            };
            let left = runs.iter().map(|r| r.x).fold(f32::MAX, f32::min);
            assert!(left >= 0.0);
            assert!(
                left + l.width <= measure + 0.5,
                "overflow: {} + {} > {measure}",
                left,
                l.width
            );
            if l.justified {
                justified += 1;
                // ends at the right margin (block quotes: at their 2 em right indent)
                let end = left + l.width;
                let quote_end = measure - 2.0 * f32::from(style.font_px);
                assert!(
                    (end - measure).abs() < 0.5 || (end - quote_end).abs() < 0.5,
                    "justified line ends at {end}, measure {measure}"
                );
            }
            assert!(l.start < l.end);
        }
        // offsets are monotonic across lines
        for w in lines.windows(2) {
            assert!(
                w[0].end <= w[1].start,
                "{:?} {:?}",
                (w[0].start, w[0].end),
                (w[1].start, w[1].end)
            );
        }
    }
    assert!(justified > 50);
}

#[test]
fn overlong_words_are_split_without_hyphens() {
    let book = from_txt("x ".repeat(3).as_bytes(), "t");
    let mut b2 = book.clone();
    b2.chapters[0].blocks = vec![ink_epub::Block::Paragraph {
        spans: vec![ink_epub::Span::plain("A".repeat(400))],
        quote: false,
    }];
    let style = Style::default();
    let mut f = fonts();
    let measure = 500.0;
    let lines = layout_chapter(&mut f, &style, measure, &b2.chapters[0]);
    assert!(lines.len() > 3);
    let mut next = 0;
    for l in &lines {
        assert_eq!(l.start, next, "split pieces are contiguous");
        assert!(l.width <= measure);
        next = l.end;
    }
    assert_eq!(next, 400);
}

#[test]
fn reading_anchor_survives_every_font_size() {
    let book = sample_book(150);
    let mut f = fonts();
    let base = Style::default();
    let pags: Vec<_> = FONT_SIZES
        .iter()
        .map(|&px| {
            paginate(
                &mut f,
                &Style {
                    font_px: px,
                    ..base
                },
                PW3_SIZE,
                &book,
            )
        })
        .collect();
    // bigger type, more pages
    assert!(pags
        .windows(2)
        .all(|w| w[0].pages.len() <= w[1].pages.len()));
    for a in &pags {
        for p in (0..a.pages.len()).step_by(7) {
            let anchor = a.anchor_of(p);
            assert_eq!(a.page_for(anchor), p, "anchor maps back to its own page");
            for b in &pags {
                let q = b.page_for(anchor);
                let page = &b.pages[q];
                assert_eq!(page.chapter, anchor.chapter);
                assert!(
                    page.start <= anchor.offset
                        && (anchor.offset < page.end || page.start == page.end),
                    "anchor {anchor:?} not on page {q} [{}, {})",
                    page.start,
                    page.end
                );
            }
        }
    }
}

fn reader(book: Book, full_every: u32) -> Reader<SimDisplay> {
    let display = SimDisplay::new(PW3_SIZE, RefreshPolicy::new(full_every));
    Reader::open(book, fonts(), Style::default(), display)
        .unwrap()
        .0
}

#[test]
fn page_turns_update_only_minimal_dirty_regions() {
    let mut r = reader(sample_book(120), 0);
    assert_eq!(r.display().log().len(), 1, "open = one full refresh");
    assert!(r.display().log()[0].full);
    for _ in 0..8 {
        let before = r.screen().clone();
        let rep = r.next_page().unwrap().unwrap();
        let after = r.screen().clone();
        assert_eq!(
            r.display().stale_pixels(),
            0,
            "every change was made visible"
        );
        assert_eq!(r.display().panel(), &after);
        assert!(!rep.updates.is_empty() && rep.updates.iter().all(|u| !u.full));
        let mut covered = 0u64;
        for u in &rep.updates {
            let g = u.region;
            covered += g.area();
            // tight: each edge of each region touches a changed pixel
            let changed = |x: u32, y: u32| before.get(x, y) != after.get(x, y);
            assert!((g.x..g.right()).any(|x| changed(x, g.y)), "top edge");
            assert!(
                (g.x..g.right()).any(|x| changed(x, g.bottom() - 1)),
                "bottom edge"
            );
            assert!((g.y..g.bottom()).any(|y| changed(g.x, y)), "left edge");
            assert!(
                (g.y..g.bottom()).any(|y| changed(g.right() - 1, y)),
                "right edge"
            );
            // margins: nothing left of the text block or above the top margin changes
            assert!(g.x + 4 >= Style::default().margins.left);
        }
        assert!(
            covered < PW3_SIZE.area() * 9 / 10,
            "page turn is not a full-screen update"
        );
    }
    // re-showing the same page changes nothing and issues no update
    let n = r.display().log().len();
    let rep = r.goto_page(r.page()).unwrap();
    assert!(rep.updates.is_empty());
    assert_eq!(r.display().log().len(), n);
}

#[test]
fn refresh_policy_flashes_every_sixth_page_turn() {
    let mut r = reader(sample_book(200), 6);
    let mut fulls = Vec::new();
    for i in 1..=14 {
        let rep = r.next_page().unwrap().unwrap();
        if rep.updates.iter().any(|u| u.full) {
            assert!(rep.updates.len() == 1 && rep.updates[0].promoted);
            fulls.push(i);
        }
    }
    assert_eq!(fulls, vec![7, 14]);
    assert_eq!(r.display().stale_pixels(), 0);
}

#[test]
fn emulated_session_with_gestures_and_font_changes() {
    let book = sample_book(150);
    let mut r = reader(book, 6);
    let tap = |x: i32, t: u64| {
        [
            TouchEvent::Down {
                id: 1,
                pos: Point::new(x, 700),
                t_ms: t,
            },
            TouchEvent::Up {
                id: 1,
                t_ms: t + 60,
            },
        ]
    };
    let mut events = Vec::new();
    for (i, x) in [900, 900, 900, 100].into_iter().enumerate() {
        events.extend(tap(x, i as u64 * 1000));
    }
    events.extend([
        TouchEvent::Down {
            id: 2,
            pos: Point::new(900, 600),
            t_ms: 9000,
        },
        TouchEvent::Move {
            id: 2,
            pos: Point::new(300, 620),
            t_ms: 9100,
        },
        TouchEvent::Up { id: 2, t_ms: 9150 },
    ]);
    let mut input = ScriptedInput::new(events, GestureConfig::default());
    let mut pages = Vec::new();
    while let Some(g) = input.next_gesture(None).unwrap() {
        if let Some(rep) = r.handle_gesture(g).unwrap() {
            pages.push(rep.page);
        }
        assert_eq!(r.display().stale_pixels(), 0);
    }
    assert_eq!(pages, vec![1, 2, 3, 2, 3]);

    // font+ keeps the anchor on screen, font- returns to exactly the same page (no drift)
    let page_before = r.page();
    let anchor = r.current_anchor();
    let up = r.step_font(1).unwrap();
    assert!(
        up.updates.len() == 1 && up.updates[0].full,
        "font change = full refresh"
    );
    let p = &r.pagination().pages[r.page()];
    assert!(p.start <= anchor.offset && anchor.offset < p.end);
    assert_eq!(r.current_anchor(), anchor);
    r.step_font(1).unwrap();
    r.step_font(-1).unwrap();
    r.step_font(-1).unwrap();
    assert_eq!(r.page(), page_before);
    assert_eq!(r.display().stale_pixels(), 0);

    // navigation after a font change moves the anchor again
    r.next_page().unwrap();
    assert_ne!(r.current_anchor(), anchor);
    let ch2 = Anchor {
        chapter: 1,
        offset: 0,
    };
    let rep = r.goto_anchor(ch2).unwrap();
    assert_eq!(r.pagination().pages[rep.page].chapter, 1);
    assert_eq!(r.pagination().pages[rep.page].start, 0);
}

/// The page holding `a` in `pag`, and whether its first line holds `a` (or is the first after it).
fn page_of(pag: &ink_render::Pagination, a: Anchor) -> (usize, bool) {
    let i = pag.page_for(a);
    let p = &pag.pages[i];
    let first = p.lines.first().is_none_or(|l| l.line.end > a.offset);
    (i, p.chapter == a.chapter && p.start <= a.offset && first)
}

#[test]
fn the_page_at_an_anchor_is_the_paginators_page_there() {
    let book = sample_book(150);
    let mut f = fonts();
    let base = Style::default();
    let normal = paginate(&mut f, &base, PW3_SIZE, &book);
    for px in [28u16, 40, 56, 72] {
        let style = Style {
            font_px: px,
            ..base
        };
        // Anchors at old page starts (what a reader holds), mid-line and at a chapter's very start.
        let mut anchors: Vec<Anchor> = (0..normal.pages.len())
            .step_by(5)
            .map(|i| normal.anchor_of(i))
            .collect();
        anchors.extend([
            Anchor {
                chapter: 2,
                offset: 917,
            },
            Anchor {
                chapter: 0,
                offset: 0,
            },
            Anchor {
                chapter: 1,
                offset: u32::MAX,
            },
        ]);
        for a in anchors {
            let shown = ink_render::layout::pages_at(
                &mut f,
                &style,
                PW3_SIZE,
                &book,
                a,
                &mut ink_render::NoImages,
                2,
            );
            assert!(!shown.is_empty(), "{px} {a:?}");
            let mut lazy = ink_render::layout::Paginator::new(
                style,
                PW3_SIZE,
                ink_render::layout::break_of(&shown[0]),
            );
            assert!(lazy.step(&mut f, &book, &mut ink_render::NoImages, None));
            let whole = lazy.into_pagination();
            let i = whole.pages.iter().position(|p| *p == shown[0]);
            let i =
                i.unwrap_or_else(|| panic!("{px} {a:?}: the shown page is not in the pagination"));
            assert_eq!(&whole.pages[i..i + shown.len()], &shown[..], "{px} {a:?}");
            if a.offset < u32::MAX {
                assert_eq!(page_of(&whole, a), (i, true), "{px} {a:?}");
            }
            // Pages tile each chapter: every page ends where the next starts.
            for w in whole.pages.windows(2) {
                if w[0].chapter == w[1].chapter {
                    assert_eq!(w[0].end, w[1].start);
                }
            }
        }
    }
}

#[test]
fn a_paginator_in_small_steps_gives_what_it_gives_at_once() {
    let book = sample_book(150);
    let mut f = fonts();
    let style = Style::default();
    let brk = Some((2, 900, 900));
    let mut once = ink_render::layout::Paginator::new(style, PW3_SIZE, brk);
    assert!(once.step(&mut f, &book, &mut ink_render::NoImages, None));
    let mut steps = ink_render::layout::Paginator::new(style, PW3_SIZE, brk);
    let mut n = 0;
    // A deadline already past: one paragraph per step.
    while !steps.step(
        &mut f,
        &book,
        &mut ink_render::NoImages,
        Some(std::time::Instant::now()),
    ) {
        n += 1;
    }
    assert!(n > 100, "{n} steps");
    assert_eq!(steps.into_pagination(), once.into_pagination());
    // Without a break it is paginate().
    let mut plain = ink_render::layout::Paginator::new(style, PW3_SIZE, None);
    plain.step(&mut f, &book, &mut ink_render::NoImages, None);
    assert_eq!(
        plain.into_pagination(),
        paginate(&mut f, &style, PW3_SIZE, &book)
    );
}

#[test]
fn a_quick_style_change_shows_the_page_at_the_anchor_then_paginates_the_rest() {
    let book = sample_book(150);
    assert!(book.chapters.len() > 3);
    let bigger = Style {
        font_px: Style::default().step_font(1),
        ..Style::default()
    };
    let mut r = reader(book.clone(), 0);
    let at = r.pagination().anchor_of(37);
    r.goto_anchor(at).unwrap();
    let first = r.set_style_quick(bigger).unwrap();
    assert!(r.layout_pending());
    assert!(
        first.updates.len() == 1 && first.updates[0].full,
        "one full refresh"
    );
    assert_eq!((first.page, first.pages), (0, 2), "the page and the next");
    let shown = r.pagination().pages[0].clone();
    assert!(shown.start <= at.offset && shown.lines[0].line.end > at.offset);
    assert_eq!(r.current_anchor(), at);
    // No number in the footer while the rest is not paginated.
    let (w, h) = (PW3_SIZE.w as usize, PW3_SIZE.h as usize);
    let footer = (h - Style::default().margins.bottom as usize) * w;
    let ink = |img: &GrayImage| {
        (footer..img.data.len())
            .filter(|&i| i % w > w / 2 && img.data[i] < 0x80)
            .count()
    };
    assert_eq!(ink(r.screen()), 0, "no number");
    let text = r.screen().data[..footer].to_vec();

    // A little at a time, then the number comes with a partial update; the text stays.
    let mut steps = 0;
    while !r.layout_step(std::time::Duration::ZERO) {
        steps += 1;
    }
    assert!(steps > 10 && !r.layout_pending());
    let whole = r.page_count();
    assert!(whole > 2);
    assert_eq!(r.pagination().pages[r.page()], shown);
    let rest = r.finish_layout().unwrap().expect("the number");
    assert!(rest.updates.iter().all(|u| !u.full));
    assert!(ink(r.screen()) > 0, "the number");
    assert_eq!(r.screen().data[..footer], text[..]);
    assert!(r.finish_layout().unwrap().is_none(), "nothing left");
    assert_eq!(r.display().stale_pixels(), 0);

    // A page turn while it is pending paginates the rest first and goes on from the page shown.
    r.set_style_quick(Style::default()).unwrap();
    let rep = r.next_page().unwrap().unwrap();
    assert!(!r.layout_pending());
    assert_eq!(rep.pages, r.page_count());
    assert!(r.pagination().pages[rep.page].start > at.offset);

    // Font+ then font- (quick) comes back to exactly the page it left.
    let mut r = reader(book.clone(), 0);
    r.goto_anchor(at).unwrap();
    let before = r.screen().data.clone();
    r.set_style_quick(bigger).unwrap();
    r.set_style_quick(Style::default()).unwrap();
    r.finish_layout().unwrap();
    assert_eq!(r.screen().data, before);

    // A one-chapter book works the same.
    let mut one = reader(from_txt(b"Just one chapter of text.", "One"), 0);
    one.set_style_quick(bigger).unwrap();
    one.finish_layout().unwrap();
    assert_eq!(one.page_count(), 1);
}

#[test]
fn chapters_paginate_alone_as_in_the_whole_book() {
    let book = sample_book(150);
    let mut f = fonts();
    let style = Style::default();
    let whole = paginate(&mut f, &style, PW3_SIZE, &book);
    let mut joined = Vec::new();
    for ci in 0..book.chapters.len() {
        joined.extend(ink_render::paginate_chapter_with(
            &mut f,
            &style,
            PW3_SIZE,
            &book,
            ci,
            &mut ink_render::NoImages,
        ));
    }
    assert_eq!(joined, whole.pages);
}

#[test]
fn a_bad_face_falls_back_to_the_regular_one() {
    let mut f = fonts();
    let bad = std::env::temp_dir().join(format!("ink-bad-face-{}.ttf", std::process::id()));
    std::fs::write(&bad, b"not a font").unwrap();
    // A file that does not parse counts as missing: the regular face draws it.
    f.set_face(ink_render::Face::Italic, &bad).unwrap();
    assert!(!f.has_face(ink_render::Face::Italic));
    let gone = std::env::temp_dir().join("ink-no-such-face.ttf");
    assert!(f.set_face(ink_render::Face::Bold, &gone).is_err());
    assert!(!f.has_face(ink_render::Face::Bold));
    assert!(f.measure(ink_render::Face::Italic, 30, "still drawn") > 0.0);
    f.set_face(ink_render::Face::Italic, std::path::Path::new(FONT))
        .unwrap();
    assert!(f.has_face(ink_render::Face::Italic));
    std::fs::remove_file(&bad).unwrap();
}
