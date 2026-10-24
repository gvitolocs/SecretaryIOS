//! EPUB/TXT/Markdown fixtures are built inside the tests (no binary fixtures, no copyrighted text).
use ink_epub::epub::{read, Limits};
use ink_epub::text::{from_markdown, from_txt};
use ink_epub::{Block, Error, ListMarker, SourceFormat, Span, Unsupported};
use std::io::{Cursor, Write};
use zip::write::FileOptions;
use zip::CompressionMethod;

const CONTAINER: &str = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#;

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    w.start_file(
        "mimetype",
        FileOptions::default().compression_method(CompressionMethod::Stored),
    )
    .unwrap();
    w.write_all(b"application/epub+zip").unwrap();
    for (name, data) in files {
        w.start_file(
            *name,
            FileOptions::default().compression_method(CompressionMethod::Deflated),
        )
        .unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn xhtml(body: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n\
         <html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\">\
         <head><title>x</title><link rel=\"stylesheet\" href=\"../style.css\"/></head><body>{body}</body></html>"
    )
}

const OPF3: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="id">urn:uuid:0000</dc:identifier>
    <dc:title>A Tiny &amp; Valid Book</dc:title>
    <dc:creator>Ada Writer</dc:creator>
    <dc:language>en</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
    <item id="c1" href="text/ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="c2" href="text/ch%202.xhtml" media-type="application/xhtml+xml"/>
    <item id="css" href="style.css" media-type="text/css"/>
    <item id="cover" href="images/cover.jpg" media-type="image/jpeg"/>
  </manifest>
  <spine><itemref idref="c1"/><itemref idref="c2" linear="yes"/></spine>
</package>"#;

const NAV: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body>
<nav epub:type="landmarks"><ol><li><a href="text/ch1.xhtml">Landmark, not TOC</a></li></ol></nav>
<nav epub:type="toc" id="toc"><h1>Contents</h1>
  <ol>
    <li><a href="text/ch1.xhtml">Beginning</a>
      <ol><li><a href="text/ch%202.xhtml#sec">The <em>Second</em> Part</a></li></ol>
    </li>
  </ol>
</nav></body></html>"#;

fn tiny_epub3(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let ch1 = xhtml(
        "<h1>One</h1><p>It was a <em>dark</em> and <b>stormy</b> night.</p>\
         <ul><li>first</li><li>second</li></ul><p><img src=\"../images/cover.jpg\" alt=\"Cover\"/></p>",
    );
    let ch2 = xhtml("<p id=\"sec\">No heading here&#8230;</p>");
    let mut files: Vec<(&str, &[u8])> = vec![
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("OEBPS/content.opf", OPF3.as_bytes()),
        ("OEBPS/nav.xhtml", NAV.as_bytes()),
        ("OEBPS/text/ch1.xhtml", ch1.as_bytes()),
        ("OEBPS/text/ch 2.xhtml", ch2.as_bytes()),
        ("OEBPS/style.css", b"p { margin: 0 }"),
    ];
    files.extend_from_slice(extra);
    zip_of(&files)
}

#[test]
fn tiny_valid_epub3_with_nav_toc() {
    let book = read(Cursor::new(tiny_epub3(&[])), &Limits::default()).unwrap();
    assert_eq!(book.format, SourceFormat::Epub);
    assert_eq!(book.title.as_deref(), Some("A Tiny & Valid Book"));
    assert_eq!(book.authors, vec!["Ada Writer"]);
    assert_eq!(book.language.as_deref(), Some("en"));
    assert!(book.warnings.is_empty(), "{:?}", book.warnings);
    assert_eq!(book.chapters.len(), 2);
    let c1 = &book.chapters[0];
    assert_eq!(c1.href, "OEBPS/text/ch1.xhtml");
    assert_eq!(c1.title.as_deref(), Some("One"));
    assert_eq!(c1.blocks[1].text(), "It was a dark and stormy night.");
    assert!(c1.blocks[1].spans()[1].style.italic);
    assert!(c1.blocks[1].spans()[3].style.bold);
    assert!(matches!(
        c1.blocks[2],
        Block::ListItem {
            marker: ListMarker::Bullet,
            depth: 1,
            ..
        }
    ));
    assert_eq!(
        c1.blocks[4],
        Block::Image {
            alt: "Cover".into(),
            src: "../images/cover.jpg".into()
        }
    );
    // chapter without a heading takes its title from the TOC
    assert_eq!(book.chapters[1].title.as_deref(), Some("The Second Part"));
    assert_eq!(book.chapters[1].blocks[0].text(), "No heading here\u{2026}");
    let toc: Vec<(&str, u8, Option<usize>)> = book
        .toc
        .iter()
        .map(|t| (t.title.as_str(), t.depth, t.chapter))
        .collect();
    assert_eq!(
        toc,
        vec![("Beginning", 0, Some(0)), ("The Second Part", 1, Some(1))]
    );
    // offsets: every block's length adds up
    assert_eq!(
        c1.char_len(),
        c1.blocks.iter().map(|b| b.char_len()).sum::<u32>()
    );
}

#[test]
fn epub2_with_ncx_toc_and_opf_at_root() {
    let opf = r#"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Old Style</dc:title></metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
    <item id="a" href="a.html" media-type="application/xhtml+xml"/>
    <item id="b" href="sub/b.html" media-type="application/xhtml+xml"/>
  </manifest>
  <spine toc="ncx"><itemref idref="a"/><itemref idref="b"/><itemref idref="missing"/></spine>
</package>"#;
    let ncx = r#"<?xml version="1.0"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1"><navMap>
  <navPoint id="n1" playOrder="1"><navLabel><text>Part One</text></navLabel><content src="a.html"/>
    <navPoint id="n2" playOrder="2"><navLabel><text>  Chapter
      Two </text></navLabel><content src="sub/b.html#x"/></navPoint>
  </navPoint>
</navMap></ncx>"#;
    let container = CONTAINER.replace("OEBPS/content.opf", "content.opf");
    let a = xhtml("<h2>Part One</h2><p>Alpha.</p>");
    let b = xhtml("<p>Beta.</p>");
    let data = zip_of(&[
        ("META-INF/container.xml", container.as_bytes()),
        ("content.opf", opf.as_bytes()),
        ("toc.ncx", ncx.as_bytes()),
        ("a.html", a.as_bytes()),
        ("sub/b.html", b.as_bytes()),
    ]);
    let book = read(Cursor::new(data), &Limits::default()).unwrap();
    assert_eq!(book.chapters.len(), 2);
    assert_eq!(book.warnings.len(), 1, "missing spine item is a warning");
    let toc: Vec<(&str, u8, Option<usize>)> = book
        .toc
        .iter()
        .map(|t| (t.title.as_str(), t.depth, t.chapter))
        .collect();
    assert_eq!(
        toc,
        vec![("Part One", 0, Some(0)), ("Chapter Two", 1, Some(1))]
    );
    assert_eq!(book.chapters[1].title.as_deref(), Some("Chapter Two"));
}

#[test]
fn adobe_drm_encryption_xml_is_refused() {
    let enc = br#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.w3.org/2001/04/xmlenc#aes128-cbc"/>
    <KeyInfo xmlns="http://www.w3.org/2000/09/xmldsig#"><resource xmlns="http://ns.adobe.com/adept">urn:uuid:1</resource></KeyInfo>
    <enc:CipherData><enc:CipherReference URI="OEBPS/text/ch1.xhtml"/></enc:CipherData>
  </enc:EncryptedData></encryption>"#;
    let err = read(
        Cursor::new(tiny_epub3(&[("META-INF/encryption.xml", enc)])),
        &Limits::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Unsupported(Unsupported::Drm)), "{err}");
    assert!(err.to_string().contains("DRM"));
}

#[test]
fn rights_xml_is_refused() {
    let err = read(
        Cursor::new(tiny_epub3(&[("META-INF/rights.xml", b"<rights/>")])),
        &Limits::default(),
    )
    .unwrap_err();
    assert!(matches!(err, Error::Unsupported(Unsupported::Drm)));
}

#[test]
fn font_obfuscation_alone_is_not_drm() {
    let enc = br#"<?xml version="1.0"?>
<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#">
  <enc:EncryptedData><enc:EncryptionMethod Algorithm="http://www.idpf.org/2008/embedding"/>
    <enc:CipherData><enc:CipherReference URI="OEBPS/fonts/a.otf"/></enc:CipherData></enc:EncryptedData>
</encryption>"#;
    let book = read(
        Cursor::new(tiny_epub3(&[("META-INF/encryption.xml", enc)])),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(book.chapters.len(), 2);
}

#[test]
fn malformed_zips_are_invalid() {
    for data in [
        b"definitely not a zip file at all, just some bytes".to_vec(),
        Vec::new(),
        {
            let mut d = tiny_epub3(&[]);
            d.truncate(d.len() / 2);
            d
        },
        {
            // central directory present but entries garbage
            let mut d = tiny_epub3(&[]);
            for b in d.iter_mut().take(200).skip(30) {
                *b = 0x55;
            }
            d
        },
    ] {
        let r = read(Cursor::new(data), &Limits::default());
        assert!(
            matches!(r, Err(Error::Invalid(_))),
            "expected Invalid, got {r:?}"
        );
    }
}

#[test]
fn zip_bomb_and_caps() {
    // 32 MiB of zeros deflates to ~32 KiB: ratio ~1000:1
    let zeros = vec![0u8; 32 * 1024 * 1024];
    let bomb = tiny_epub3(&[("OEBPS/text/ch1.xhtml.bak", &zeros)]);
    let mut opf_bomb = OPF3.replace("<spine>", "<spine><itemref idref=\"bomb\"/>");
    opf_bomb = opf_bomb.replace(
        "<manifest>",
        "<manifest><item id=\"bomb\" href=\"text/ch1.xhtml.bak\" media-type=\"application/xhtml+xml\"/>",
    );
    let data = {
        let ch1 = xhtml("<p>x</p>");
        zip_of(&[
            ("META-INF/container.xml", CONTAINER.as_bytes()),
            ("OEBPS/content.opf", opf_bomb.as_bytes()),
            ("OEBPS/nav.xhtml", NAV.as_bytes()),
            ("OEBPS/text/ch1.xhtml", ch1.as_bytes()),
            ("OEBPS/text/ch 2.xhtml", ch1.as_bytes()),
            ("OEBPS/text/ch1.xhtml.bak", &zeros),
        ])
    };
    let err = read(Cursor::new(data), &Limits::default()).unwrap_err();
    assert!(matches!(err, Error::Limit(_)), "{err}");
    // unreferenced bomb entries are never inflated
    assert!(read(Cursor::new(bomb), &Limits::default()).is_ok());

    // per-entry and total caps
    let small = Limits {
        max_entry_bytes: 300,
        ..Limits::default()
    };
    assert!(matches!(
        read(Cursor::new(tiny_epub3(&[])), &small),
        Err(Error::Limit(_))
    ));
    let total = Limits {
        max_total_bytes: 2_000,
        ..Limits::default()
    };
    assert!(matches!(
        read(Cursor::new(tiny_epub3(&[])), &total),
        Err(Error::Limit(_))
    ));
    let few = Limits {
        max_entries: 4,
        ..Limits::default()
    };
    assert!(matches!(
        read(Cursor::new(tiny_epub3(&[])), &few),
        Err(Error::Limit(_))
    ));
}

#[test]
fn malformed_chapter_xml_is_a_warning_with_partial_content() {
    let opf = OPF3.replace("<itemref idref=\"c2\" linear=\"yes\"/>", "");
    let bad = "<html><body><p>kept</p><p>lost <!-- unterminated";
    let data = zip_of(&[
        ("META-INF/container.xml", CONTAINER.as_bytes()),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/nav.xhtml", NAV.as_bytes()),
        ("OEBPS/text/ch1.xhtml", bad.as_bytes()),
    ]);
    let book = read(Cursor::new(data), &Limits::default()).unwrap();
    assert_eq!(book.chapters[0].blocks[0].text(), "kept");
    assert!(book.warnings.iter().any(|w| w.contains("ch1.xhtml")));
}

#[test]
fn path_traversal_rootfile_is_rejected() {
    let container = CONTAINER.replace("OEBPS/content.opf", "../../etc/passwd");
    let data = zip_of(&[("META-INF/container.xml", container.as_bytes())]);
    assert!(matches!(
        read(Cursor::new(data), &Limits::default()),
        Err(Error::Invalid(_))
    ));
}

#[test]
fn txt_paragraphs_chapters_and_encodings() {
    let txt = "\u{feff}Some front matter\nwrapped here.\r\n\r\nCHAPTER I.\r\n\r\nFirst   para\nof one.\n\nSecond.\n\nChapter 2\n\nThird.\n";
    let b = from_txt(txt.as_bytes(), "novel");
    assert_eq!(b.title.as_deref(), Some("novel"));
    assert_eq!(b.chapters.len(), 3);
    assert_eq!(
        b.chapters[0].blocks[0].text(),
        "Some front matter wrapped here."
    );
    assert_eq!(b.chapters[1].title.as_deref(), Some("CHAPTER I."));
    assert_eq!(b.chapters[1].blocks[1].text(), "First para of one.");
    assert_eq!(b.chapters[2].blocks[1].text(), "Third.");

    // no blank lines: one paragraph per line; Windows-1252 fallback
    let b = from_txt(b"caf\xe9 au lait\nsecond line\n", "x");
    assert_eq!(b.chapters[0].blocks.len(), 2);
    assert_eq!(b.chapters[0].blocks[0].text(), "caf\u{e9} au lait");

    // empty input still yields one (empty) chapter
    assert_eq!(from_txt(b"", "e").chapters.len(), 1);
}

#[test]
fn markdown_blocks() {
    let md = "# The Title\n\nIntro with *em* and **strong**\ncontinued line.\n\n## Section\n\n- one\n- two\n  - nested\n1. first\n2. second\n\n> quoted\n> text\n\n```\ncode  kept\n  indented\n```\n\n---\n\n![A figure](img/f.png)\n\n# Second Chapter\n\nEnd.\n";
    let b = from_markdown(md.as_bytes(), "fallback");
    assert_eq!(b.format, SourceFormat::Markdown);
    assert_eq!(b.title.as_deref(), Some("The Title"));
    assert_eq!(b.chapters.len(), 2);
    let c = &b.chapters[0].blocks;
    assert_eq!(
        c[0],
        Block::Heading {
            level: 1,
            spans: vec![Span::plain("The Title")]
        }
    );
    assert_eq!(c[1].text(), "Intro with em and strong continued line.");
    assert!(c[1].spans()[1].style.italic && c[1].spans()[3].style.bold);
    assert!(matches!(c[2], Block::Heading { level: 2, .. }));
    let items: Vec<(ListMarker, u8, String)> = c
        .iter()
        .filter_map(|b| match b {
            Block::ListItem { marker, depth, .. } => Some((*marker, *depth, b.text())),
            _ => None,
        })
        .collect();
    assert_eq!(
        items,
        vec![
            (ListMarker::Bullet, 1, "one".into()),
            (ListMarker::Bullet, 1, "two".into()),
            (ListMarker::Bullet, 2, "nested".into()),
            (ListMarker::Number(1), 1, "first".into()),
            (ListMarker::Number(2), 1, "second".into()),
        ]
    );
    assert!(c.contains(&Block::Paragraph {
        spans: vec![Span::plain("quoted text")],
        quote: true
    }));
    assert!(c.contains(&Block::Preformatted {
        text: "code  kept\n  indented".into()
    }));
    assert!(c.contains(&Block::Rule));
    assert!(c.contains(&Block::Image {
        alt: "A figure".into(),
        src: "img/f.png".into()
    }));
    assert_eq!(b.chapters[1].title.as_deref(), Some("Second Chapter"));
}

#[test]
fn open_dispatches_by_extension() {
    let dir = std::env::temp_dir().join(format!("ink-epub-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("book.epub");
    std::fs::write(&p, tiny_epub3(&[])).unwrap();
    assert_eq!(ink_epub::open(&p).unwrap().chapters.len(), 2);
    let p = dir.join("notes.md");
    std::fs::write(&p, "# Hi\n\nThere.").unwrap();
    assert_eq!(ink_epub::open(&p).unwrap().title.as_deref(), Some("Hi"));
    let p = dir.join("x.pdf");
    std::fs::write(&p, "%PDF").unwrap();
    assert!(matches!(
        ink_epub::open(&p),
        Err(Error::Unsupported(Unsupported::Format))
    ));
    std::fs::remove_dir_all(&dir).unwrap();
}
