//! The wire format of Cua's `cua.env.v1` API as Lucina speaks it: gRPC-web
//! framing and just enough protobuf for the messages it reads or rewrites (the
//! viewer's clipboard sync in `computer_view`, files in `computer_files`).
//! Field numbers come from the descriptors embedded in Cua's `viewer.js`.

/// gRPC-web framing: `[flags][u32 big-endian length][bytes]` per frame; flag
/// `0x80` marks the trailers frame, flag `1` a compressed message.
pub(crate) mod grpc_web {
    pub(crate) fn frames(mut b: &[u8]) -> Option<Vec<(u8, &[u8])>> {
        let mut out = Vec::new();
        while !b.is_empty() {
            let (&flags, rest) = b.split_first()?;
            let len = u32::from_be_bytes(rest.get(..4)?.try_into().ok()?) as usize;
            let bytes = rest.get(4..4 + len)?;
            out.push((flags, bytes));
            b = &rest[4 + len..];
        }
        Some(out)
    }

    /// The one uncompressed message of a unary request.
    pub(crate) fn single_message(b: &[u8]) -> Option<&[u8]> {
        match frames(b)?.as_slice() {
            [(0, m)] => Some(m),
            _ => None,
        }
    }

    pub(crate) fn frame(flags: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + bytes.len());
        out.push(flags);
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
        out
    }

    /// Whether a trailers frame says `grpc-status: 0`.
    pub(crate) fn status_ok(trailers: &[u8]) -> bool {
        String::from_utf8_lossy(trailers).split("\r\n").any(|l| {
            l.split_once(':').is_some_and(|(k, v)| {
                k.trim().eq_ignore_ascii_case("grpc-status") && v.trim() == "0"
            })
        })
    }
}

pub(crate) async fn read_capped(mut up: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = up.chunk().await.ok()? {
        if out.len() + chunk.len() > limit {
            return None;
        }
        out.extend_from_slice(&chunk);
    }
    Some(out)
}

/// Just enough protobuf for the clipboard messages of `cua.env.v1` (field
/// numbers from the viewer's embedded descriptor):
///
/// - `ClipboardContent { optional string text = 1; repeated string file_paths = 2; optional bytes image_png = 3; }`
/// - `SetClipboardRequest { ClipboardContent content = 1; }`
/// - `GetClipboardResponse { ClipboardContent content = 1; uint64 generation = 2; }`
/// - `SetClipboardResponse { uint64 generation = 1; }`
pub(crate) mod clip_proto {
    pub(crate) enum Field<'a> {
        Varint(u64),
        Len(&'a [u8]),
        Fixed,
    }

    pub(crate) fn varint(b: &mut &[u8]) -> Option<u64> {
        let mut v = 0u64;
        for i in 0..10 {
            let (&x, rest) = b.split_first()?;
            *b = rest;
            v |= u64::from(x & 0x7f) << (7 * i);
            if x & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    /// Every field of a message, or `None` if it is not well-formed.
    pub(crate) fn fields(mut b: &[u8]) -> Option<Vec<(u64, Field<'_>)>> {
        let mut out = Vec::new();
        while !b.is_empty() {
            let key = varint(&mut b)?;
            let field = match key & 7 {
                0 => Field::Varint(varint(&mut b)?),
                1 | 5 => {
                    let n = if key & 7 == 1 { 8 } else { 4 };
                    b = b.get(n..)?;
                    Field::Fixed
                }
                2 => {
                    let n = usize::try_from(varint(&mut b)?).ok()?;
                    let v = b.get(..n)?;
                    b = &b[n..];
                    Field::Len(v)
                }
                _ => return None,
            };
            out.push((key >> 3, field));
        }
        Some(out)
    }

    /// A `ClipboardContent`, borrowed from the message it came in.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub(crate) struct Content<'a> {
        pub(crate) text: Option<&'a str>,
        pub(crate) files: Vec<&'a str>,
        pub(crate) png: Option<&'a [u8]>,
    }

    impl Content<'_> {
        pub(crate) fn is_empty(&self) -> bool {
            self.text.is_none() && self.files.is_empty() && self.png.is_none()
        }
    }

    /// `None` if malformed.
    fn content(b: &[u8]) -> Option<Content<'_>> {
        let mut c = Content::default();
        for (n, f) in fields(b)? {
            match (n, f) {
                (1, Field::Len(v)) => c.text = Some(std::str::from_utf8(v).ok()?),
                (2, Field::Len(v)) => c.files.push(std::str::from_utf8(v).ok()?),
                (3, Field::Len(v)) => c.png = Some(v),
                _ => {}
            }
        }
        Some(c)
    }

    /// The content of a `SetClipboardRequest`.
    pub(crate) fn set_request(b: &[u8]) -> Option<Content<'_>> {
        let mut c = Content::default();
        for (n, f) in fields(b)? {
            if let (1, Field::Len(v)) = (n, f) {
                c = content(v)?;
            }
        }
        Some(c)
    }

    /// The generation a `SetClipboardResponse { uint64 generation = 1; }` reports.
    pub(crate) fn set_response_generation(b: &[u8]) -> Option<u64> {
        fields(b)?.into_iter().find_map(|(n, f)| match (n, f) {
            (1, Field::Varint(g)) => Some(g),
            _ => None,
        })
    }

    /// `(content, generation)` of a `GetClipboardResponse`.
    pub(crate) fn get_response(b: &[u8]) -> Option<(Content<'_>, u64)> {
        let (mut c, mut generation) = (Content::default(), 0);
        for (n, f) in fields(b)? {
            match (n, f) {
                (1, Field::Len(v)) => c = content(v)?,
                (2, Field::Varint(g)) => generation = g,
                _ => {}
            }
        }
        Some((c, generation))
    }

    pub(crate) fn put_varint(out: &mut Vec<u8>, mut v: u64) {
        while v >= 0x80 {
            out.push((v as u8) | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    pub(crate) fn put_len(out: &mut Vec<u8>, field: u64, v: &[u8]) {
        put_varint(out, field << 3 | 2);
        put_varint(out, v.len() as u64);
        out.extend_from_slice(v);
    }

    fn content_bytes(c: &Content<'_>) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(t) = c.text {
            put_len(&mut out, 1, t.as_bytes());
        }
        for f in &c.files {
            put_len(&mut out, 2, f.as_bytes());
        }
        if let Some(p) = c.png {
            put_len(&mut out, 3, p);
        }
        out
    }

    pub(crate) fn set_request_bytes(c: &Content<'_>) -> Vec<u8> {
        let mut out = Vec::new();
        put_len(&mut out, 1, &content_bytes(c));
        out
    }

    pub(crate) fn get_response_bytes(c: &Content<'_>, generation: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_len(&mut out, 1, &content_bytes(c));
        if generation != 0 {
            put_varint(&mut out, 2 << 3);
            put_varint(&mut out, generation);
        }
        out
    }

    /// Whether these bytes start like a PNG file.
    pub(crate) fn is_png(b: &[u8]) -> bool {
        b.starts_with(b"\x89PNG\r\n\x1a\n")
    }
}
