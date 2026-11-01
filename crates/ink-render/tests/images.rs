//! Pictures through an `Images` source: box sizes, anchors across pagination with and without pictures,
//! full-page pictures, drawing outcomes and the reader's prefetch / redraw.
//! Uses the OFL Liberation Serif fixture (see fixtures/LICENSE-LiberationSerif-OFL.txt).
use ink_epub::text::from_markdown;
use ink_epub::{Block, Book, Chapter};
use ink_kindle_hal::sim::SimDisplay;
use ink_kindle_hal::{GrayImage, Rect, RefreshPolicy, PW3_SIZE};
use ink_render::layout::{content_height, LineContent, Page, Pagination};
use ink_render::render::{render_page, render_page_with};
use ink_render::{
    image_box, paginate, paginate_with, Anchor, DrawOutcome, ImageRequest, ImageSize, Images,
    NoImages, PageImages, Reader, Style,
};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

const FONT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/LiberationSerif-Regular.ttf"
);

fn fonts() -> ink_render::Fonts {
    ink_render::Fonts::load(std::path::Path::new(FONT)).expect("fixture font")
}

#[derive(Default)]
struct State {
    sizes: HashMap<String, ImageSize>,
    pending: HashSet<String>,
    draws: Vec<(u32, String, Rect)>,
    prefetches: Vec<Vec<ImageRequest>>,
}

/// Fake picture source: known sizes, "pending" pictures, and a log of calls. Ready pictures are drawn black.
#[derive(Clone, Default)]
struct Fake(Rc<RefCell<State>>);

impl Fake {
    fn with(sizes: &[(&str, u32, u32)]) -> Self {
        let f = Fake::default();
        for &(src, w, h) in sizes {
            f.0.borrow_mut()
                .sizes
                .insert(src.to_owned(), ImageSize { w, h });
        }
        f
    }
}

impl Images for Fake {
    fn size(&mut self, _chapter: u32, src: &str) -> Option<ImageSize> {
        self.0.borrow().sizes.get(src).copied()
    }
    fn draw(&mut self, chapter: u32, src: &str, img: &mut GrayImage, dst: Rect) -> DrawOutcome {
        let mut s = self.0.borrow_mut();
        s.draws.push((chapter, src.to_owned(), dst));
        if !s.sizes.contains_key(src) {
            DrawOutcome::Missing
        } else if s.pending.contains(src) {
            DrawOutcome::Pending
        } else {
            img.fill_rect(dst, 0x00);
            DrawOutcome::Drawn
        }
    }
    fn prefetch(&mut self, wanted: &[ImageRequest]) {
        self.0.borrow_mut().prefetches.push(wanted.to_vec());
    }
}

fn measure(style: &Style) -> f32 {
    (PW3_SIZE.w - style.margins.left - style.margins.right) as f32
}

/// The same deterministic prose as `layout_and_reader.rs` (headings, lists, quotes, a long word, an image).
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

/// One chapter: `before` paragraphs of text, the picture `src`, `after` paragraphs.
fn picture_book(before: usize, src: &str, after: usize) -> Book {
    let para = "The lantern swung over the harbour while she remembered the old silver letters \
                beneath the candle, and the river ran quietly to the mountain in the evening.\n\n";
    let mut s = para.repeat(before);
    s.push_str(&format!("![A picture]({src})\n\n"));
    s.push_str(&para.repeat(after));
    let mut book = from_markdown(s.as_bytes(), "Pictures");
    assert_eq!(book.chapters.len(), 1);
    book.title = Some("Pictures".into());
    book
}

/// The (page index, image line y, w, h) of the first picture in `pag`.
fn find_picture(pag: &Pagination) -> (usize, i32, u32, u32) {
    for (i, p) in pag.pages.iter().enumerate() {
        for pl in &p.lines {
            if let LineContent::Image { w, h, .. } = pl.line.content {
                return (i, pl.y, w, h);
            }
        }
    }
    panic!("no picture");
}

fn box_rect(style: &Style, y: i32, w: u32, h: u32) -> Rect {
    let cw = measure(style) as u32;
    Rect::new(
        style.margins.left + (cw - w) / 2,
        style.margins.top + y as u32,
        w,
        h,
    )
}

/// The page shows a placeholder: far more 0xee pixels than anti-aliased text edges produce.
fn has_placeholder(img: &GrayImage) -> bool {
    img.data.iter().filter(|&&p| p == 0xee).count() > 20_000
}

#[test]
fn image_boxes() {
    let m = 928.0;
    let mh = 1284;
    let sz = |w, h| ImageSize { w, h };
    assert_eq!(image_box(sz(100, 50), m, mh, false), (200, 100));
    assert_eq!(image_box(sz(2000, 500), m, mh, false), (928, 232));
    let (w, h) = image_box(sz(300, 3000), m, mh, false);
    assert_eq!(h, 1284);
    assert_eq!(w, 128);
    assert_eq!(image_box(sz(200, 300), m, mh, true), (856, 1284));
    // Not full page: never above 2× intrinsic.
    assert_eq!(image_box(sz(200, 300), m, mh, false), (400, 600));
    assert_eq!(image_box(sz(0, 300), m, mh, false), (0, 0));
    assert_eq!(image_box(sz(300, 0), m, mh, true), (0, 0));
    // Extreme ratios keep at least one pixel.
    assert_eq!(image_box(sz(100_000, 1), m, mh, false), (928, 1));
}

#[test]
fn no_images_pagination_is_unchanged() {
    let book = sample_book(120);
    let style = Style::default();
    let mut f = fonts();
    let a = paginate(&mut f, &style, PW3_SIZE, &book);
    let b = paginate_with(&mut f, &style, PW3_SIZE, &book, &mut NoImages);
    assert_eq!(a, b);
    // The placeholder box is two thirds of the column and four lines high.
    let (_, _, w, h) = find_picture(&a);
    assert_eq!(w, measure(&style) as u32 * 2 / 3);
    let body_line = (f32::from(style.font_px) * style.line_height).round() as u32;
    assert_eq!(h, body_line * 4);
}

#[test]
fn anchors_survive_pictures() {
    let book = picture_book(6, "tall.png", 30);
    let style = Style::default();
    let mut f = fonts();
    let plain = paginate(&mut f, &style, PW3_SIZE, &book);
    let mut fake = Fake::with(&[("tall.png", 300, 3000)]);
    let pics = paginate_with(&mut f, &style, PW3_SIZE, &book, &mut fake);
    assert_ne!(
        plain.pages.len(),
        pics.pages.len(),
        "the picture moves pages"
    );
    let (pic_page, _, w, h) = find_picture(&pics);
    assert_eq!(h as i32, content_height(PW3_SIZE, &style));
    assert_eq!(w, 128);
    let image_offset = pics.pages[pic_page].lines.iter().find_map(|pl| {
        matches!(pl.line.content, LineContent::Image { .. }).then_some(pl.line.start)
    });
    let image_offset = image_offset.unwrap();
    let check = |pag: &Pagination, a: Anchor| {
        let p: &Page = &pag.pages[pag.page_for(a)];
        assert_eq!(p.chapter, a.chapter);
        assert!(p.start <= a.offset && a.offset < p.end, "{a:?} in {p:?}");
    };
    let mut tested = 0;
    for k in 0..plain.pages.len() {
        let a = plain.anchor_of(k);
        if a.offset > image_offset {
            check(&pics, a);
            check(
                &pics,
                Anchor {
                    offset: a.offset + 5,
                    ..a
                },
            );
            tested += 1;
        }
    }
    assert!(tested >= 2);
    check(
        &pics,
        Anchor {
            chapter: 0,
            offset: image_offset,
        },
    );
    // The picture keeps its one offset.
    let l = &pics.pages[pic_page]
        .lines
        .iter()
        .find(|pl| matches!(pl.line.content, LineContent::Image { .. }))
        .unwrap()
        .line;
    assert_eq!(l.end, l.start + 1);
}

#[test]
fn full_page_picture_fills_the_content_area() {
    let style = Style::default();
    let mut book = Book::new(ink_epub::SourceFormat::Epub);
    book.chapters.push(Chapter {
        title: None,
        href: "cover.xhtml".into(),
        blocks: vec![
            Block::Paragraph {
                spans: vec![ink_epub::Span::plain("  ")],
                quote: false,
            },
            Block::Image {
                alt: "Cover".into(),
                src: "images/cover.jpg".into(),
            },
        ],
    });
    // A picture with a captioned heading is not full page.
    book.chapters.push(Chapter {
        title: None,
        href: "c1.xhtml".into(),
        blocks: vec![
            Block::Heading {
                level: 1,
                spans: vec![ink_epub::Span::plain("One")],
            },
            Block::Image {
                alt: String::new(),
                src: "images/cover.jpg".into(),
            },
        ],
    });
    let mut fake = Fake::with(&[("images/cover.jpg", 200, 300)]);
    let mut f = fonts();
    let pag = paginate_with(&mut f, &style, PW3_SIZE, &book, &mut fake);
    let cover: Vec<_> = pag.pages.iter().filter(|p| p.chapter == 0).collect();
    assert_eq!(cover.len(), 1, "one page");
    let pl = cover[0]
        .lines
        .iter()
        .find(|pl| matches!(pl.line.content, LineContent::Image { .. }))
        .unwrap();
    assert_eq!(
        pl.line.content,
        LineContent::Image {
            block: 1,
            w: 856,
            h: 1284
        }
    );
    assert_eq!(pl.line.space_before, 0);
    assert_eq!(pl.y, 0);
    assert_eq!(pl.line.height, content_height(PW3_SIZE, &style));
    let ch1 = pag.pages.iter().find(|p| p.chapter == 1).unwrap();
    let img = ch1
        .lines
        .iter()
        .find_map(|pl| match pl.line.content {
            LineContent::Image { w, h, .. } => Some((w, h)),
            _ => None,
        })
        .unwrap();
    assert_eq!(img, (400, 600), "2× intrinsic, not full page");
}

#[test]
fn drawing_outcomes() {
    let book = picture_book(1, "pic.png", 1);
    let style = Style::default();
    let mut f = fonts();
    let mut fake = Fake::with(&[("pic.png", 200, 150)]);
    let pag = paginate_with(&mut f, &style, PW3_SIZE, &book, &mut fake);
    let (page, y, w, h) = find_picture(&pag);
    assert_eq!((w, h), (400, 300));
    let r = box_rect(&style, y, w, h);
    let mut img = GrayImage::new(PW3_SIZE, 0x55);

    // Drawn.
    let rep = render_page_with(&mut img, &mut f, &book, &pag, page, &mut fake);
    assert_eq!(
        rep,
        PageImages {
            drawn: 1,
            pending: 0,
            missing: 0
        }
    );
    assert_eq!(
        fake.0.borrow().draws.last().unwrap(),
        &(0, "pic.png".to_owned(), r)
    );
    for yy in r.y..r.bottom() {
        assert!(img.row(yy)[r.x as usize..r.right() as usize]
            .iter()
            .all(|&p| p == 0));
    }
    assert!(!has_placeholder(&img), "no placeholder");

    // Pending: light frame, white inside.
    fake.0.borrow_mut().pending.insert("pic.png".into());
    let rep = render_page_with(&mut img, &mut f, &book, &pag, page, &mut fake);
    assert_eq!(rep.pending, 1);
    assert_eq!(img.get(r.x, r.y), 0xcc);
    assert_eq!(img.get(r.x + 1, r.y + 1), 0xcc);
    assert_eq!(img.get(r.right() - 1, r.bottom() - 1), 0xcc);
    for yy in r.y + 2..r.bottom() - 2 {
        assert!(img.row(yy)[r.x as usize + 2..r.right() as usize - 2]
            .iter()
            .all(|&p| p == 0xff));
    }

    // Missing (the source lost the picture after layout): placeholder in the line's box.
    fake.0.borrow_mut().sizes.clear();
    let rep = render_page_with(&mut img, &mut f, &book, &pag, page, &mut fake);
    assert_eq!(rep.missing, 1);
    assert_eq!(img.get(r.x, r.y), 0x88);
    assert_eq!(img.get(r.x + 3, r.y + 3), 0xee);
    assert_eq!(img.get(r.right() - 1, r.bottom() - 1), 0x88);
    let label_ink = (r.y..r.bottom()).any(|yy| {
        img.row(yy)[r.x as usize..r.right() as usize]
            .iter()
            .any(|&p| p < 0x80)
    });
    assert!(label_ink, "label drawn");

    // Without a source, the output is today's.
    let sample = sample_book(120);
    let plain = paginate(&mut f, &style, PW3_SIZE, &sample);
    let (pic_page, _, _, _) = find_picture(&plain);
    for page in [0, pic_page, plain.pages.len() - 1] {
        let mut a = GrayImage::new(PW3_SIZE, 0);
        let mut b = GrayImage::new(PW3_SIZE, 0);
        render_page(&mut a, &mut f, &sample, &plain, page);
        let rep = render_page_with(&mut b, &mut f, &sample, &plain, page, &mut NoImages);
        assert_eq!(a, b, "page {page}");
        if page == pic_page {
            assert_eq!(rep.missing, 1);
            assert!(has_placeholder(&a));
        }
    }
}

#[test]
fn reader_prefetches_and_redraws_pending_pictures() {
    let book = picture_book(12, "pic.png", 12);
    let style = Style::default();
    let fake = Fake::with(&[("pic.png", 200, 150)]);
    fake.0.borrow_mut().pending.insert("pic.png".into());

    // From the start: the picture is a page or two ahead and gets prefetched.
    let display = SimDisplay::new(PW3_SIZE, RefreshPolicy::new(0));
    let (mut r, rep) = Reader::open_at_with(
        book.clone(),
        fonts(),
        style,
        display,
        None,
        Box::new(fake.clone()),
    )
    .unwrap();
    let (pic_page, y, w, h) = find_picture(r.pagination());
    assert!((1..=2).contains(&pic_page), "picture on page {pic_page}");
    assert_eq!(rep.images, PageImages::default());
    let wanted = fake.0.borrow().prefetches.last().unwrap().clone();
    assert_eq!(wanted, r.page_images(pic_page));
    assert_eq!(
        wanted,
        vec![ImageRequest {
            chapter: 0,
            block: 12,
            src: "pic.png".into(),
            w,
            h
        }]
    );
    assert!(r.page_images(r.page_count() + 3).is_empty());
    assert!(!r.pending_images());

    // At the picture, still decoding.
    let image_offset = r.pagination().pages[pic_page]
        .lines
        .iter()
        .find_map(|pl| {
            matches!(pl.line.content, LineContent::Image { .. }).then_some(pl.line.start)
        })
        .unwrap();
    let anchor = Anchor {
        chapter: 0,
        offset: image_offset,
    };
    let rep = r.goto_anchor(anchor).unwrap();
    assert_eq!(rep.page, pic_page);
    assert_eq!(rep.images.pending, 1);
    assert!(r.pending_images());
    let pr = box_rect(&style, y, w, h);
    assert_eq!(r.screen().get(pr.x, pr.y), 0xcc);
    // The previous page has no pictures; the request list stays on the next pages.
    assert!(fake.0.borrow().prefetches.last().unwrap().is_empty());

    // Decoded: redraw updates the picture's box, page and anchor stay.
    fake.0.borrow_mut().pending.clear();
    let rep = r.redraw().unwrap();
    assert_eq!(
        rep.images,
        PageImages {
            drawn: 1,
            pending: 0,
            missing: 0
        }
    );
    assert!(!r.pending_images());
    assert_eq!(r.page(), pic_page);
    assert_eq!(r.current_anchor(), anchor);
    assert!(!rep.updates.is_empty());
    assert!(rep.updates.iter().all(|u| !u.full));
    let covered = rep
        .updates
        .iter()
        .fold(Rect::default(), |a, u| a.union(&u.region));
    assert!(covered.contains_rect(&pr), "{covered:?} covers {pr:?}");
    assert_eq!(r.screen().get(pr.x, pr.y), 0x00);
    assert_eq!(r.display().panel().get(pr.x, pr.y), 0x00);

    // A bigger font re-paginates with the pictures and keeps the anchor.
    let rep = r.step_font(2).unwrap();
    assert_eq!(rep.anchor, anchor);
    assert_eq!(r.current_anchor(), anchor);
    let p = &r.pagination().pages[r.page()];
    assert!(p.start <= anchor.offset && anchor.offset < p.end);
    assert_eq!(rep.images.drawn, 1);

    // Opening with the plain reader is unchanged.
    let display = SimDisplay::new(PW3_SIZE, RefreshPolicy::new(0));
    let (plain, _) = Reader::open(book.clone(), fonts(), style, display).unwrap();
    let display = SimDisplay::new(PW3_SIZE, RefreshPolicy::new(0));
    let (with_none, _) =
        Reader::open_at_with(book, fonts(), style, display, None, Box::new(NoImages)).unwrap();
    assert_eq!(plain.pagination(), with_none.pagination());
    assert_eq!(plain.screen(), with_none.screen());
}
