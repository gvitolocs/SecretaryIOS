// Share-to-Lucina kernels, part 1: the file router, the receive limits and the file-name
// sanitizer (POST /v1/share, docs/share.md, docs/attachments.md).
//
// Production (branch lucina-rs, commit 485d658): crates/lucina-server/src/share/
//   Word                 <-> every string literal router.rs and timeline.rs compare against;
//                            `Unknown` is any other string (the `_` arms)
//   kinds                <-> router::KINDS
//   attachment_kinds     <-> lucina-store/src/timeline.rs ATTACHMENT_KINDS (normalize's whitelist)
//   Input.sniff          <-> router::sniff(head) (sniff_range: what it can return)
//   Input.ext            <-> router::ext_kind(lowercased Path::extension(name)), including the
//                            handlers::code::language fallback (ext_range)
//   Input.xls_numbers    <-> matches!(ext, "xls" | "numbers")
//   Input.mime           <-> router::mime_kind(mime) (mime_range)
//   Input.texty          <-> router::texty(head)
//   Input.zip / Zip      <-> head starts with PK\x03\x04, and the class router::zip_kind finds
//                            (word/, xl/, ppt/, an ODF mimetype, other, unreadable = None)
//   route                <-> router::route (the decision after reading the head)
//   timeline_kind        <-> router::timeline_kind
//   keep_whitelisted     <-> timeline::normalize's attachment filter + take(32) for shares
//   FieldIn / step / run <-> the multipart loop of mod.rs::receive (count + check_parts, the
//                            per-chunk saturating `total`, the per-field caps, blank text/url,
//                            repeats, unknown names)
//   finish               <-> the meta.location part (+ check_parts(parts.len() + 1) since
//                            485d658) and "at least one text, url or file"
//   valid_id / valid_session / Scalars <-> mod.rs valid_id, valid_session, and the note/lang/
//                            source/title character limits after the loop
//   sanitize             <-> mod.rs::sanitize (map, replace(".."), trim_matches(['.', ' ']),
//                            "file", the 120-byte cut that keeps a <= 20-byte extension)
// Voice notes (branch voice-notes, commit 438d748):
//   route_for            <-> router::route_for (`container`: head[4..8] == "ftyp" or the
//                            Matroska magic 1A 45 DF A3)
//   timeline_kind_for    <-> router::timeline_kind_for
//   received_kind        <-> the attachment kind mod.rs::receive stores for a file part
//   extract_handler      <-> extract::read's route_for(.., kind == "voice") and the `voice`
//   / voice_subtype          argument of extract::audio (subtype "voice")
//   b64_decoded_len      <-> base64 0.22.1 STANDARD, DecodePaddingMode::Indifferent,
//                            trailing bits rejected: ENGINE.decode(s).map(|b| b.len())
//   valid_waveform       <-> lucina-store timeline::valid_waveform (after `trim`)
//   clip16 / stored_transcript <-> timeutil::clip_utf16 / clean_string(.., MAX_TRANSCRIPT_CHARS)
//                            (after js_trim); `clip` above <-> share/mod.rs::clip (chars)
// Differential test: crates/lucina-server/tests/verus_share.rs (branch lucina-rs): the real
// route/sniff/timeline_kind/normalize over every magic header x extension x mime (and random
// names), the real sanitize over random and adversarial names, and the real receive endpoint
// at the part, byte, field and id limits. On branch voice-notes (0ce757c) the same file also
// runs the real route_for / timeline_kind_for over every header x extension x mime x voice
// flag, valid_waveform and the base64 engine against b64_decoded_len / valid_waveform, and
// normalize's stored transcripts against clip16.
//
// Proven:
//   SK1.1 route_total_in_kinds: for every well-formed input, route returns one kind, and it
//         is in KINDS (never "file", "voice" or another string). route is an exec function
//         with no precondition, so it is total.
//   SK1.2 bytes_win: without a zip container a decisive sniff result is the answer,
//         whatever the name and mime type say.
//   SK1.3 zip_rule: a zip container is document/sheet/slides/archive: the extension's
//         kind when it is one of those, else what is inside.
//   SK1.4 text_claims_need_text: a text-like claim (text, code, sheet, calendar, contact) by
//         name or mime on non-text bytes routes to "other", except a sheet named .xls or
//         .numbers; text and code are only ever returned for text bytes.
//   SK1.5 timeline_kind_whitelisted: timeline_kind of every string (so of every route result)
//         is in ATTACHMENT_KINDS, as are the "text", "link" and "location" parts receive
//         builds, so normalize's filter never drops a shared attachment;
//         normalize_keeps_share_attachments: with at most 32 parts (SK4.1) its take(32) keeps
//         them all either.
//   SK4.1 parts_at_most_32: an accepted request has at most 32 parts, with the location
//         check of 485d658 and without it; location_check_never_fires: both rules accept
//         exactly the same requests (the meta field that carries the location is itself one
//         of the 32 counted fields).
//   SK4.2 bytes_bounded: an accepted request's chunks sum to at most MAX_BYTES (2 GiB), the
//         saturating per-chunk total equals the true sum, every text field is <= 3 MiB,
//         meta <= 64 KiB, every other form field <= 16 KiB, and a url <= 8192 bytes.
//   SK4.3 scalars_bounded / valid_id_is_one_path_component / uuid_share_ids_are_valid: the
//         accepted shareId is 8-64 of [A-Za-z0-9-] (never "." / "..", no separator), the
//         default uuid is valid, sessionId is 0-64 of [A-Za-z0-9_-], note <= 4000 chars,
//         lang <= 32, source <= 120, title <= 1000.
//   SK1.6 voice_routing / timeline_kind_for_whitelisted: the voice flag keeps route total and
//         in KINDS; it changes the handler only for an audio-MIME MP4/Matroska container that
//         sniff calls video (to audio); the timeline kind is `voice` exactly for a flagged part
//         routed audio; a flagged part not routed audio gets the unflagged timeline kind; every
//         result passes normalize's whitelist.
//   SK1.7 receive_and_extract_agree: extract::read, re-routing with the flag read back from
//         the stored kind, picks receive's handler, and exactly the parts stored as `voice` run
//         the audio handler with subtype voice.
//   SK4.5 valid_waveform_exactly: valid_waveform keeps exactly the trimmed strings that
//         base64-decode to 1..=64 bytes (the 88-character pre-check never rejects one), so a
//         stored waveform is 1..=88 bytes of base64 for 1..=64 levels; waveform_limits_witness:
//         88 chars can decode to 66 bytes (both checks are needed), 86 unpadded / 88 padded
//         chars decode to 64 and are kept. voice_transcript_bounded: a stored transcript
//         (attachment or meta.transcript) is non-empty, <= 4000 UTF-16 units (so <= 4000
//         chars) and a prefix of its input; clip16 after the 4000-char clip of receive and
//         process keeps what clip16 alone keeps (normalize's js_trim in between can still
//         drop whitespace left at that cut).
//   SK4.4 sanitize_fixed_safe: with the proposed fix, sanitize's output is non-empty, at
//         most 120 bytes, without '/', '\\', ':' or control characters, and has no "..";
//         sanitize_safe_but_dotdot: today's sanitize has every property except the last.
//
// Counterexample (bug on lucina-rs, unfixed_sanitize_keeps_dotdot): when the 120-byte cut
// ends right after a '.', sanitize appends ".ext" to it: "a"*115 + "." + "b"*10 + ".jpg"
// (130 bytes) becomes "a"*115 + "..jpg". Fix: after the cut, drop trailing dots before
// pushing the extension (`while s.ends_with('.') { s.pop(); }`). Not a path traversal (the
// name has no separator and is prefixed with "NN-"), but it breaks the documented
// "no '..'" property that the unit test names_limits_and_wording asserts for short names.
//
// Refuted (SK4.1): "an accepted share can have 33 parts because meta.location is pushed
// after the 32-part check". The location comes from the meta field, which itself counts
// toward the 32 fields, so the parts before it are at most 31. The check added in 485d658
// is harmless defence in depth but never fires.
//
// No assume, admit or external_body.
use vstd::prelude::*;

verus! {

// ================================================================ SK1: router

/// The string literals in play. `Unknown` is any other string.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Word {
    Image,
    Video,
    Audio,
    Pdf,
    Document,
    Slides,
    Sheet,
    Text,
    Code,
    Archive,
    Calendar,
    Contact,
    Location,
    Link,
    Other,
    File,
    Voice,
    Unknown,
}

/// router::KINDS
pub open spec fn kinds() -> Seq<Word> {
    seq![
        Word::Image,
        Word::Video,
        Word::Audio,
        Word::Pdf,
        Word::Document,
        Word::Slides,
        Word::Sheet,
        Word::Text,
        Word::Code,
        Word::Archive,
        Word::Calendar,
        Word::Contact,
        Word::Location,
        Word::Link,
        Word::Other,
    ]
}

/// timeline.rs ATTACHMENT_KINDS
pub open spec fn attachment_kinds() -> Seq<Word> {
    seq![
        Word::Image,
        Word::Voice,
        Word::File,
        Word::Video,
        Word::Audio,
        Word::Pdf,
        Word::Document,
        Word::Link,
        Word::Text,
        Word::Contact,
        Word::Location,
    ]
}

/// What router::sniff can return.
pub open spec fn sniff_range(w: Word) -> bool {
    matches!(w, Word::Pdf | Word::Image | Word::Video | Word::Audio | Word::Archive | Word::Document
        | Word::Calendar | Word::Contact)
}

/// What router::ext_kind can return (the code fallback returns "code").
pub open spec fn ext_range(w: Word) -> bool {
    matches!(w, Word::Image | Word::Video | Word::Audio | Word::Pdf | Word::Document | Word::Slides
        | Word::Sheet | Word::Text | Word::Archive | Word::Calendar | Word::Contact | Word::Code)
}

/// What router::mime_kind can return.
pub open spec fn mime_range(w: Word) -> bool {
    matches!(w, Word::Pdf | Word::Calendar | Word::Contact | Word::Sheet | Word::Text | Word::Archive
        | Word::Image | Word::Video | Word::Audio | Word::Document | Word::Slides | Word::Code)
}

/// The class of a zip container's entries, in zip_kind's order of checks.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Zip {
    /// an entry under word/
    WordDir,
    /// xl/
    XlDir,
    /// ppt/
    PptDir,
    /// a `mimetype` entry ending in opendocument.text / .spreadsheet / .presentation
    OdText,
    OdSheet,
    OdPresentation,
    /// another `mimetype`
    OdOther,
    /// a readable zip without any of these
    Plain,
    /// ZipArchive::new failed (zip_kind returns None)
    Unreadable,
}

#[derive(Clone, Copy)]
pub struct Input {
    pub zip: Option<Zip>,
    pub sniff: Option<Word>,
    pub ext: Option<Word>,
    pub xls_numbers: bool,
    pub mime: Option<Word>,
    pub texty: bool,
}

pub open spec fn well_formed(i: Input) -> bool {
    &&& (i.sniff matches Some(w) ==> sniff_range(w))
    &&& (i.ext matches Some(w) ==> ext_range(w))
    &&& (i.mime matches Some(w) ==> mime_range(w))
    &&& (i.xls_numbers ==> i.ext == Some(Word::Sheet))
}

pub open spec fn zip_kind_spec(z: Zip) -> Option<Word> {
    match z {
        Zip::WordDir => Some(Word::Document),
        Zip::XlDir => Some(Word::Sheet),
        Zip::PptDir => Some(Word::Slides),
        Zip::OdText => Some(Word::Document),
        Zip::OdSheet => Some(Word::Sheet),
        Zip::OdPresentation => Some(Word::Slides),
        Zip::OdOther => Some(Word::Archive),
        Zip::Plain => Some(Word::Archive),
        Zip::Unreadable => None,
    }
}

pub open spec fn office(w: Word) -> bool {
    matches!(w, Word::Document | Word::Sheet | Word::Slides | Word::Archive)
}

pub open spec fn text_claim(w: Word) -> bool {
    matches!(w, Word::Text | Word::Code | Word::Sheet | Word::Calendar | Word::Contact)
}

pub open spec fn by_name(i: Input) -> Option<Word> {
    match i.ext {
        Some(k) => Some(k),
        None => i.mime,
    }
}

pub open spec fn route_spec(i: Input) -> Word {
    match i.zip {
        Some(z) => match i.ext {
            Some(k) if office(k) => k,
            _ => match zip_kind_spec(z) {
                Some(k) => k,
                None => Word::Archive,
            },
        },
        None => match i.sniff {
            Some(k) => k,
            None => match by_name(i) {
                Some(k) => if text_claim(k) && !i.texty {
                    if k == Word::Sheet && i.xls_numbers {
                        k
                    } else {
                        Word::Other
                    }
                } else {
                    k
                },
                None => if i.texty {
                    Word::Text
                } else {
                    Word::Other
                },
            },
        },
    }
}

pub fn zip_kind(z: Zip) -> (r: Option<Word>)
    ensures
        r == zip_kind_spec(z),
{
    match z {
        Zip::WordDir => Some(Word::Document),
        Zip::XlDir => Some(Word::Sheet),
        Zip::PptDir => Some(Word::Slides),
        Zip::OdText => Some(Word::Document),
        Zip::OdSheet => Some(Word::Sheet),
        Zip::OdPresentation => Some(Word::Slides),
        Zip::OdOther => Some(Word::Archive),
        Zip::Plain => Some(Word::Archive),
        Zip::Unreadable => None,
    }
}

/// router::route after the head, the extension and the mime type are classified.
pub fn route(i: &Input) -> (r: Word)
    ensures
        r == route_spec(*i),
{
    if let Some(z) = i.zip {
        // PK\x03\x04: an office extension decides, else the entries.
        return match i.ext {
            Some(Word::Document) => Word::Document,
            Some(Word::Sheet) => Word::Sheet,
            Some(Word::Slides) => Word::Slides,
            Some(Word::Archive) => Word::Archive,
            _ => match zip_kind(z) {
                Some(k) => k,
                None => Word::Archive,
            },
        };
    }
    if let Some(k) = i.sniff {
        return k;
    }
    let by_name = match i.ext {
        Some(k) => Some(k),
        None => i.mime,
    };
    match by_name {
        Some(k) => {
            let claim = matches!(k, Word::Text | Word::Code | Word::Sheet | Word::Calendar | Word::Contact);
            if claim && !i.texty {
                if matches!(k, Word::Sheet) && i.xls_numbers {
                    k
                } else {
                    Word::Other
                }
            } else {
                k
            }
        },
        None => if i.texty {
            Word::Text
        } else {
            Word::Other
        },
    }
}

pub open spec fn timeline_kind_spec(w: Word) -> Word {
    match w {
        Word::Image => Word::Image,
        Word::Video => Word::Video,
        Word::Audio => Word::Audio,
        Word::Pdf => Word::Pdf,
        Word::Document | Word::Slides | Word::Sheet => Word::Document,
        Word::Text => Word::Text,
        Word::Contact => Word::Contact,
        Word::Location => Word::Location,
        Word::Link => Word::Link,
        _ => Word::File,
    }
}

/// router::timeline_kind
pub fn timeline_kind(w: Word) -> (r: Word)
    ensures
        r == timeline_kind_spec(w),
{
    match w {
        Word::Image => Word::Image,
        Word::Video => Word::Video,
        Word::Audio => Word::Audio,
        Word::Pdf => Word::Pdf,
        Word::Document | Word::Slides | Word::Sheet => Word::Document,
        Word::Text => Word::Text,
        Word::Contact => Word::Contact,
        Word::Location => Word::Location,
        Word::Link => Word::Link,
        _ => Word::File,
    }
}

pub open spec fn is_kind(w: Word) -> bool {
    !matches!(w, Word::File | Word::Voice | Word::Unknown)
}

pub proof fn lemma_kinds_exactly(w: Word)
    ensures
        kinds().contains(w) <==> is_kind(w),
{
    let k = kinds();
    match w {
        Word::Image => assert(k[0] == w),
        Word::Video => assert(k[1] == w),
        Word::Audio => assert(k[2] == w),
        Word::Pdf => assert(k[3] == w),
        Word::Document => assert(k[4] == w),
        Word::Slides => assert(k[5] == w),
        Word::Sheet => assert(k[6] == w),
        Word::Text => assert(k[7] == w),
        Word::Code => assert(k[8] == w),
        Word::Archive => assert(k[9] == w),
        Word::Calendar => assert(k[10] == w),
        Word::Contact => assert(k[11] == w),
        Word::Location => assert(k[12] == w),
        Word::Link => assert(k[13] == w),
        Word::Other => assert(k[14] == w),
        _ => {
            assert forall|j: int| 0 <= j < k.len() implies k[j] != w by {}
        },
    }
}

pub proof fn lemma_attachment_kinds_exactly(w: Word)
    ensures
        attachment_kinds().contains(w) <==> matches!(w, Word::Image | Word::Voice | Word::File
            | Word::Video | Word::Audio | Word::Pdf | Word::Document | Word::Link | Word::Text
            | Word::Contact | Word::Location),
{
    let a = attachment_kinds();
    match w {
        Word::Image => assert(a[0] == w),
        Word::Voice => assert(a[1] == w),
        Word::File => assert(a[2] == w),
        Word::Video => assert(a[3] == w),
        Word::Audio => assert(a[4] == w),
        Word::Pdf => assert(a[5] == w),
        Word::Document => assert(a[6] == w),
        Word::Link => assert(a[7] == w),
        Word::Text => assert(a[8] == w),
        Word::Contact => assert(a[9] == w),
        Word::Location => assert(a[10] == w),
        _ => {
            assert forall|j: int| 0 <= j < a.len() implies a[j] != w by {}
        },
    }
}

/// SK1.1: one kind, always in KINDS.
pub proof fn route_total_in_kinds(i: Input)
    requires
        well_formed(i),
    ensures
        kinds().contains(route_spec(i)),
        is_kind(route_spec(i)),
{
    lemma_kinds_exactly(route_spec(i));
}

/// SK1.2: a decisive sniff result is never overridden by the name or the mime type.
pub proof fn bytes_win(i: Input, j: Input, k: Word)
    requires
        i.zip is None,
        i.sniff == Some(k),
        j.zip is None,
        j.sniff == Some(k),
    ensures
        route_spec(i) == k,
        route_spec(j) == route_spec(i),
{
}

/// SK1.3: zip containers.
pub proof fn zip_rule(i: Input)
    requires
        i.zip is Some,
    ensures
        office(route_spec(i)),
        (i.ext is Some && office(i.ext->0)) ==> route_spec(i) == i.ext->0,
        !(i.ext is Some && office(i.ext->0)) ==> route_spec(i) == match zip_kind_spec(i.zip->0) {
            Some(k) => k,
            None => Word::Archive,
        },
{
}

/// SK1.4: text-like claims must be backed by text bytes.
pub proof fn text_claims_need_text(i: Input)
    requires
        well_formed(i),
    ensures
        (i.zip is None && i.sniff is None && !i.texty && by_name(i) is Some && text_claim(by_name(i)->0)
            && !(by_name(i)->0 == Word::Sheet && i.xls_numbers)) ==> route_spec(i) == Word::Other,
        (route_spec(i) == Word::Text || route_spec(i) == Word::Code) ==> i.zip is None && i.sniff is None
            && i.texty,
        (route_spec(i) == Word::Calendar || route_spec(i) == Word::Contact) ==> i.zip is None && (
        i.sniff == Some(route_spec(i)) || i.texty),
        (route_spec(i) == Word::Sheet) ==> i.zip is Some || i.texty || i.xls_numbers,
        route_spec(i) == Word::Other ==> i.zip is None && i.sniff is None,
{
}

/// SK1.5: every timeline kind is whitelisted, so normalize never drops a share attachment.
pub proof fn timeline_kind_whitelisted(w: Word, i: Input)
    ensures
        attachment_kinds().contains(timeline_kind_spec(w)),
        attachment_kinds().contains(timeline_kind_spec(route_spec(i))),
        // the kinds receive gives non-file parts
        attachment_kinds().contains(Word::Text),
        attachment_kinds().contains(Word::Link),
        attachment_kinds().contains(Word::Location),
{
    lemma_attachment_kinds_exactly(timeline_kind_spec(w));
    lemma_attachment_kinds_exactly(timeline_kind_spec(route_spec(i)));
    lemma_attachment_kinds_exactly(Word::Text);
    lemma_attachment_kinds_exactly(Word::Link);
    lemma_attachment_kinds_exactly(Word::Location);
}

/// normalize: `.filter(ATTACHMENT_KINDS.contains).take(32)` for a share item.
pub open spec fn keep_whitelisted(s: Seq<Word>) -> Seq<Word>
    decreases s.len(),
{
    if s.len() == 0 {
        s
    } else if attachment_kinds().contains(s.last()) {
        keep_whitelisted(s.drop_last()).push(s.last())
    } else {
        keep_whitelisted(s.drop_last())
    }
}

pub open spec fn normalize_attachments(s: Seq<Word>) -> Seq<Word> {
    let k = keep_whitelisted(s);
    if k.len() > 32 {
        k.take(32)
    } else {
        k
    }
}

proof fn lemma_keep_all(s: Seq<Word>)
    requires
        forall|i: int| 0 <= i < s.len() ==> attachment_kinds().contains(#[trigger] s[i]),
    ensures
        keep_whitelisted(s) == s,
    decreases s.len(),
{
    if s.len() > 0 {
        let d = s.drop_last();
        assert forall|i: int| 0 <= i < d.len() implies attachment_kinds().contains(#[trigger] d[i]) by {
            assert(d[i] == s[i]);
        }
        lemma_keep_all(d);
        assert(attachment_kinds().contains(s[s.len() - 1]));
        assert(d.push(s.last()) =~= s);
    }
}

/// SK1.5 (with SK4.1): the attachments of an accepted share (at most 32, kinds from
/// timeline_kind or text/link/location) all survive normalize.
pub proof fn normalize_keeps_share_attachments(s: Seq<Word>)
    requires
        s.len() <= 32,
        forall|i: int| 0 <= i < s.len() ==> attachment_kinds().contains(#[trigger] s[i]),
    ensures
        normalize_attachments(s) == s,
{
    lemma_keep_all(s);
}

// ================================================================ SK4: receive limits

pub open spec fn max_parts() -> nat {
    32
}

pub open spec fn max_bytes() -> nat {
    2 * 1024 * 1024 * 1024
}

pub open spec fn usize_max() -> nat {
    0xffff_ffff_ffff_ffff
}

#[derive(PartialEq, Eq, Clone, Copy)]
pub enum FieldKind {
    File,
    Text,
    Url,
    Meta,
    /// shareId, intent, note, lang, sessionId, channel, source, title (`name` says which)
    Scalar,
    Unknown,
}

pub struct FieldIn {
    pub kind: FieldKind,
    pub name: nat,
    pub chunks: Seq<nat>,
    /// text/url whose value trims to empty (skipped)
    pub blank: bool,
    /// meta that parses with a valid `location`
    pub location: bool,
}

pub struct Acc {
    pub count: nat,
    pub parts: nat,
    pub total: nat,
    pub meta: bool,
    pub location: bool,
    pub seen: Set<nat>,
}

pub open spec fn cap(k: FieldKind) -> Option<nat> {
    match k {
        FieldKind::File => None,
        FieldKind::Text => Some(3 * 1024 * 1024),
        FieldKind::Meta => Some(64 * 1024),
        _ => Some(16 * 1024),
    }
}

pub open spec fn sat_add(a: nat, b: nat) -> nat {
    if a + b > usize_max() {
        usize_max()
    } else {
        a + b
    }
}

pub open spec fn sum(cs: Seq<nat>) -> nat
    decreases cs.len(),
{
    if cs.len() == 0 {
        0
    } else {
        cs[0] + sum(cs.drop_first())
    }
}

/// The chunk loop: `total = total.saturating_add(len)`, 413 past MAX_BYTES, then the
/// field cap (`bytes.len() + chunk.len() > cap`). Returns (total, field bytes).
pub open spec fn chunks(total: nat, used: nat, c: Option<nat>, cs: Seq<nat>) -> Option<(nat, nat)>
    decreases cs.len(),
{
    if cs.len() == 0 {
        Some((total, used))
    } else {
        let t = sat_add(total, cs[0]);
        if t > max_bytes() {
            None
        } else if c matches Some(k) && used + cs[0] > k {
            None
        } else {
            chunks(t, used + cs[0], c, cs.drop_first())
        }
    }
}

/// One multipart field.
pub open spec fn step(a: Acc, f: FieldIn) -> Option<Acc> {
    let count = a.count + 1;
    if count > max_parts() {
        None
    } else if f.kind == FieldKind::Unknown {
        None
    } else {
        match chunks(a.total, 0, cap(f.kind), f.chunks) {
            None => None,
            Some((total, used)) => {
                let a2 = Acc { count, total, ..a };
                match f.kind {
                    FieldKind::File => Some(Acc { parts: a.parts + 1, ..a2 }),
                    FieldKind::Text => if f.blank {
                        Some(a2)
                    } else {
                        Some(Acc { parts: a.parts + 1, ..a2 })
                    },
                    FieldKind::Url => if f.blank {
                        Some(a2)
                    } else if used > 8192 {
                        None
                    } else {
                        Some(Acc { parts: a.parts + 1, ..a2 })
                    },
                    FieldKind::Meta => if a.meta {
                        None
                    } else {
                        Some(Acc { meta: true, location: f.location, ..a2 })
                    },
                    _ => if a.seen.contains(f.name) {
                        None
                    } else {
                        Some(Acc { seen: a.seen.insert(f.name), ..a2 })
                    },
                }
            },
        }
    }
}

pub open spec fn run(a: Acc, fs: Seq<FieldIn>) -> Option<Acc>
    decreases fs.len(),
{
    if fs.len() == 0 {
        Some(a)
    } else {
        match step(a, fs[0]) {
            None => None,
            Some(b) => run(b, fs.drop_first()),
        }
    }
}

/// After the loop: the location part (with `check_parts(parts.len() + 1)` when
/// `location_check`, as since 485d658) and the non-empty rule. Returns the part count.
pub open spec fn finish(a: Acc, location_check: bool) -> Option<nat> {
    let parts = if a.location {
        if location_check && a.parts + 1 > max_parts() {
            -1int
        } else {
            (a.parts + 1) as int
        }
    } else {
        a.parts as int
    };
    if parts <= 0 {
        None
    } else {
        Some(parts as nat)
    }
}

pub open spec fn start() -> Acc {
    Acc { count: 0, parts: 0, total: 0, meta: false, location: false, seen: Set::empty() }
}

pub open spec fn accepted_parts(fs: Seq<FieldIn>, location_check: bool) -> Option<nat> {
    match run(start(), fs) {
        None => None,
        Some(a) => finish(a, location_check),
    }
}

pub open spec fn inv(a: Acc) -> bool {
    &&& a.parts + (if a.meta { 1nat } else { 0 }) <= a.count
    &&& a.count <= max_parts()
    &&& (a.location ==> a.meta)
    &&& a.total <= max_bytes()
}

pub open spec fn field_ok(f: FieldIn) -> bool {
    &&& (cap(f.kind) matches Some(k) ==> sum(f.chunks) <= k)
    &&& (f.kind == FieldKind::Url && !f.blank ==> sum(f.chunks) <= 8192)
    &&& f.kind != FieldKind::Unknown
}

pub open spec fn sum_all(fs: Seq<FieldIn>) -> nat
    decreases fs.len(),
{
    if fs.len() == 0 {
        0
    } else {
        sum(fs[0].chunks) + sum_all(fs.drop_first())
    }
}

proof fn lemma_chunks(total: nat, used: nat, c: Option<nat>, cs: Seq<nat>)
    requires
        total <= max_bytes(),
        c matches Some(k) ==> used <= k,
        chunks(total, used, c, cs) is Some,
    ensures
        ({
            let (t, u) = chunks(total, used, c, cs)->0;
            &&& t == total + sum(cs)
            &&& t <= max_bytes()
            &&& u == used + sum(cs)
            &&& (c matches Some(k) ==> u <= k)
        }),
    decreases cs.len(),
{
    if cs.len() > 0 {
        // below MAX_BYTES the saturating add is exact
        assert(sat_add(total, cs[0]) <= max_bytes());
        assert(total + cs[0] <= max_bytes());
        lemma_chunks(total + cs[0], used + cs[0], c, cs.drop_first());
    }
}

proof fn lemma_step(a: Acc, f: FieldIn)
    requires
        inv(a),
        step(a, f) is Some,
    ensures
        inv(step(a, f)->0),
        step(a, f)->0.total == a.total + sum(f.chunks),
        field_ok(f),
{
    lemma_chunks(a.total, 0, cap(f.kind), f.chunks);
}

proof fn lemma_run(a: Acc, fs: Seq<FieldIn>)
    requires
        inv(a),
        run(a, fs) is Some,
    ensures
        inv(run(a, fs)->0),
        run(a, fs)->0.total == a.total + sum_all(fs),
        forall|i: int| 0 <= i < fs.len() ==> field_ok(#[trigger] fs[i]),
    decreases fs.len(),
{
    if fs.len() > 0 {
        lemma_step(a, fs[0]);
        let b = step(a, fs[0])->0;
        let rest = fs.drop_first();
        lemma_run(b, rest);
        assert forall|i: int| 0 <= i < fs.len() implies field_ok(#[trigger] fs[i]) by {
            if i > 0 {
                assert(fs[i] == rest[i - 1]);
            }
        }
    }
}

/// SK4.1: at most 32 parts, with or without the location check; and the check never
/// changes the answer.
pub proof fn parts_at_most_32(fs: Seq<FieldIn>)
    ensures
        accepted_parts(fs, true) matches Some(n) ==> 1 <= n <= max_parts(),
        accepted_parts(fs, false) matches Some(n) ==> 1 <= n <= max_parts(),
        accepted_parts(fs, true) == accepted_parts(fs, false),
{
    if run(start(), fs) is Some {
        lemma_run(start(), fs);
    }
}

/// SK4.2: bytes.
pub proof fn bytes_bounded(fs: Seq<FieldIn>, location_check: bool)
    requires
        accepted_parts(fs, location_check) is Some,
    ensures
        sum_all(fs) <= max_bytes(),
        run(start(), fs)->0.total == sum_all(fs),
        forall|i: int| #![auto] 0 <= i < fs.len() && fs[i].kind == FieldKind::Text ==> sum(fs[i].chunks)
            <= 3 * 1024 * 1024,
        forall|i: int| #![auto] 0 <= i < fs.len() && fs[i].kind == FieldKind::Meta ==> sum(fs[i].chunks)
            <= 64 * 1024,
        forall|i: int|
            #![auto]
            0 <= i < fs.len() && (fs[i].kind == FieldKind::Url || fs[i].kind == FieldKind::Scalar) ==> sum(
                fs[i].chunks,
            ) <= 16 * 1024,
        forall|i: int| #![auto] 0 <= i < fs.len() && fs[i].kind == FieldKind::Url && !fs[i].blank ==> sum(
            fs[i].chunks,
        ) <= 8192,
{
    lemma_run(start(), fs);
}

/// valid_id: 8..=64 bytes of [A-Za-z0-9-].
pub open spec fn alnum(b: u8) -> bool {
    (0x30 <= b <= 0x39) || (0x41 <= b <= 0x5a) || (0x61 <= b <= 0x7a)
}

pub open spec fn valid_id(s: Seq<u8>) -> bool {
    8 <= s.len() <= 64 && forall|i: int| 0 <= i < s.len() ==> alnum(#[trigger] s[i]) || s[i] == 0x2d
}

pub open spec fn valid_session(s: Seq<u8>) -> bool {
    s.len() <= 64 && forall|i: int| 0 <= i < s.len() ==> alnum(#[trigger] s[i]) || s[i] == 0x2d || s[i]
        == 0x5f
}

/// The checks after the loop, on the form's scalar fields (lengths in chars).
pub struct Scalars {
    pub share_id: Seq<u8>,
    pub session: Seq<u8>,
    pub note: nat,
    pub lang: nat,
    pub source: nat,
    pub title: nat,
}

pub open spec fn scalars_ok(s: Scalars) -> bool {
    valid_id(s.share_id) && valid_session(s.session) && s.note <= 4000 && s.lang <= 32 && s.source <= 120
        && s.title <= 1000
}

/// SK4.3
pub proof fn scalars_bounded(s: Scalars)
    requires
        scalars_ok(s),
    ensures
        8 <= s.share_id.len() <= 64,
        s.session.len() <= 64,
        s.note <= 4000 && s.lang <= 32 && s.source <= 120 && s.title <= 1000,
{
}

/// SK4.3: a valid shareId is one safe path component (`month.join(id)`): no '/', '\\' or
/// '.', and at least 8 bytes, so never "." or "..".
pub proof fn valid_id_is_one_path_component(s: Seq<u8>)
    requires
        valid_id(s),
    ensures
        forall|i: int| 0 <= i < s.len() ==> #[trigger] s[i] != 0x2f && s[i] != 0x5c && s[i] != 0x2e && s[i]
            != 0,
        s.len() >= 8,
{
}

pub open spec fn lower_hex(b: u8) -> bool {
    (0x30 <= b <= 0x39) || (0x61 <= b <= 0x66)
}

/// SK4.3: the default shareId (uuid v4, hyphenated: 36 chars of [0-9a-f-]) is valid.
pub proof fn uuid_share_ids_are_valid(s: Seq<u8>)
    requires
        s.len() == 36,
        forall|i: int| 0 <= i < s.len() ==> lower_hex(#[trigger] s[i]) || s[i] == 0x2d,
    ensures
        valid_id(s),
{
}

// ================================================================ SK4.4: sanitize

/// char::is_control: general category Cc.
pub open spec fn is_control(c: char) -> bool {
    (c as u32) <= 0x1f || (0x7f <= (c as u32) && (c as u32) <= 0x9f)
}

pub open spec fn char_ok(c: char) -> bool {
    !is_control(c) && c != '/' && c != '\\' && c != ':'
}

pub open spec fn all_ok(s: Seq<char>) -> bool {
    forall|i: int| 0 <= i < s.len() ==> char_ok(#[trigger] s[i])
}

pub open spec fn no_dd(s: Seq<char>) -> bool {
    forall|i: int| 0 <= i < s.len() - 1 ==> !(#[trigger] s[i] == '.' && s[i + 1] == '.')
}

pub open spec fn map_char(c: char) -> char {
    if char_ok(c) {
        c
    } else {
        '_'
    }
}

pub open spec fn step_map(s: Seq<char>) -> Seq<char> {
    s.map_values(|c: char| map_char(c))
}

/// str::replace("..", "_"): left to right, non-overlapping.
pub open spec fn replace_dd(s: Seq<char>) -> Seq<char>
    decreases s.len(),
{
    if s.len() < 2 {
        s
    } else if s[0] == '.' && s[1] == '.' {
        seq!['_'] + replace_dd(s.subrange(2, s.len() as int))
    } else {
        seq![s[0]] + replace_dd(s.subrange(1, s.len() as int))
    }
}

pub open spec fn trimmed_char(c: char) -> bool {
    c == '.' || c == ' '
}

pub open spec fn trim_start(s: Seq<char>) -> Seq<char>
    decreases s.len(),
{
    if s.len() > 0 && trimmed_char(s[0]) {
        trim_start(s.subrange(1, s.len() as int))
    } else {
        s
    }
}

pub open spec fn trim_end(s: Seq<char>) -> Seq<char>
    decreases s.len(),
{
    if s.len() > 0 && trimmed_char(s.last()) {
        trim_end(s.drop_last())
    } else {
        s
    }
}

/// The proposed fix: drop trailing dots after the cut.
pub open spec fn trim_end_dots(s: Seq<char>) -> Seq<char>
    decreases s.len(),
{
    if s.len() > 0 && s.last() == '.' {
        trim_end_dots(s.drop_last())
    } else {
        s
    }
}

pub open spec fn utf8_len(c: char) -> nat {
    if (c as u32) < 0x80 {
        1
    } else if (c as u32) < 0x800 {
        2
    } else if (c as u32) < 0x10000 {
        3
    } else {
        4
    }
}

/// str::len (UTF-8 bytes)
pub open spec fn blen(s: Seq<char>) -> nat
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        blen(s.drop_last()) + utf8_len(s.last())
    }
}

/// clip: the first n chars.
pub open spec fn clip(s: Seq<char>, n: int) -> Seq<char> {
    if 0 <= n < s.len() {
        s.take(n)
    } else {
        s
    }
}

/// `while s.len() > lim { s.pop(); }`
pub open spec fn pop_to(s: Seq<char>, lim: int) -> Seq<char>
    decreases s.len(),
{
    if s.len() > 0 && blen(s) > lim {
        pop_to(s.drop_last(), lim)
    } else {
        s
    }
}

pub open spec fn last_dot(s: Seq<char>) -> int
    decreases s.len(),
{
    if s.len() == 0 {
        -1
    } else if s.last() == '.' {
        s.len() - 1
    } else {
        last_dot(s.drop_last())
    }
}

/// `Path::new(&s).extension()` for a name without separators: the part after the last
/// '.', unless that '.' is the first character.
pub open spec fn extension(s: Seq<char>) -> Option<Seq<char>> {
    let d = last_dot(s);
    if d > 0 {
        Some(s.subrange(d + 1, s.len() as int))
    } else {
        None
    }
}

/// `.filter(|e| e.len() <= 20).map(|e| format!(".{e}")).unwrap_or_default()`
pub open spec fn ext_suffix(s: Seq<char>) -> Seq<char> {
    match extension(s) {
        Some(e) => if blen(e) <= 20 {
            seq!['.'] + e
        } else {
            Seq::empty()
        },
        None => Seq::empty(),
    }
}

pub open spec fn file_name() -> Seq<char> {
    seq!['f', 'i', 'l', 'e']
}

pub open spec fn cleaned(raw: Seq<char>) -> Seq<char> {
    trim_end(trim_start(replace_dd(step_map(raw))))
}

/// mod.rs::sanitize; `fixed` adds the trailing-dot trim before the extension.
pub open spec fn sanitize(raw: Seq<char>, fixed: bool) -> Seq<char> {
    let s = cleaned(raw);
    if s.len() == 0 {
        file_name()
    } else if blen(s) <= 120 {
        s
    } else {
        let e = ext_suffix(s);
        let lim = 120 - blen(e);
        let cut = pop_to(clip(s, lim), lim);
        let cut = if fixed {
            trim_end_dots(cut)
        } else {
            cut
        };
        cut + e
    }
}

// ---------------------------------------------------------------- sanitize lemmas

proof fn lemma_subrange_props(s: Seq<char>, a: int, b: int)
    requires
        0 <= a <= b <= s.len(),
    ensures
        all_ok(s) ==> all_ok(s.subrange(a, b)),
        no_dd(s) ==> no_dd(s.subrange(a, b)),
{
    let t = s.subrange(a, b);
    if all_ok(s) {
        assert forall|i: int| 0 <= i < t.len() implies char_ok(#[trigger] t[i]) by {
            assert(t[i] == s[a + i]);
        }
    }
    if no_dd(s) {
        assert forall|i: int| 0 <= i < t.len() - 1 implies !(#[trigger] t[i] == '.' && t[i + 1] == '.') by {
            assert(t[i] == s[a + i]);
            assert(t[i + 1] == s[a + i + 1]);
        }
    }
}

proof fn lemma_map_ok(s: Seq<char>)
    ensures
        all_ok(step_map(s)),
        step_map(s).len() == s.len(),
{
    let m = step_map(s);
    assert forall|i: int| 0 <= i < m.len() implies char_ok(#[trigger] m[i]) by {
        assert(m[i] == map_char(s[i]));
        assert(char_ok('_'));
    }
}

proof fn lemma_replace_dd(s: Seq<char>)
    ensures
        all_ok(s) ==> all_ok(replace_dd(s)),
        no_dd(replace_dd(s)),
        s.len() > 0 ==> replace_dd(s).len() > 0,
        s.len() > 0 && s[0] != '.' ==> replace_dd(s)[0] != '.',
    decreases s.len(),
{
    let r = replace_dd(s);
    if s.len() >= 2 {
        if s[0] == '.' && s[1] == '.' {
            let t = s.subrange(2, s.len() as int);
            lemma_replace_dd(t);
            lemma_subrange_props(s, 2, s.len() as int);
            let rt = replace_dd(t);
            assert(r == seq!['_'] + rt);
            if all_ok(s) {
                assert forall|i: int| 0 <= i < r.len() implies char_ok(#[trigger] r[i]) by {
                    if i > 0 {
                        assert(r[i] == rt[i - 1]);
                    }
                }
            }
            assert forall|i: int| 0 <= i < r.len() - 1 implies !(#[trigger] r[i] == '.' && r[i + 1] == '.') by {
                if i > 0 {
                    assert(r[i] == rt[i - 1]);
                    assert(r[i + 1] == rt[i]);
                }
            }
        } else {
            let t = s.subrange(1, s.len() as int);
            lemma_replace_dd(t);
            lemma_subrange_props(s, 1, s.len() as int);
            let rt = replace_dd(t);
            assert(r == seq![s[0]] + rt);
            assert(t[0] == s[1]);
            if all_ok(s) {
                assert forall|i: int| 0 <= i < r.len() implies char_ok(#[trigger] r[i]) by {
                    if i > 0 {
                        assert(r[i] == rt[i - 1]);
                    }
                }
            }
            assert forall|i: int| 0 <= i < r.len() - 1 implies !(#[trigger] r[i] == '.' && r[i + 1] == '.') by {
                if i > 0 {
                    assert(r[i] == rt[i - 1]);
                    assert(r[i + 1] == rt[i]);
                } else {
                    assert(r[1] == rt[0]);
                }
            }
        }
    }
}

proof fn lemma_no_dd_fixpoint(s: Seq<char>)
    requires
        no_dd(s),
    ensures
        replace_dd(s) == s,
    decreases s.len(),
{
    if s.len() >= 2 {
        assert(!(s[0] == '.' && s[1] == '.'));
        let t = s.subrange(1, s.len() as int);
        lemma_subrange_props(s, 1, s.len() as int);
        lemma_no_dd_fixpoint(t);
        assert(seq![s[0]] + t =~= s);
    }
}

proof fn lemma_trim_start(s: Seq<char>)
    ensures
        exists|k: int| 0 <= k <= s.len() && trim_start(s) == s.subrange(k, s.len() as int),
        trim_start(s).len() > 0 ==> !trimmed_char(trim_start(s)[0]),
    decreases s.len(),
{
    if s.len() > 0 && trimmed_char(s[0]) {
        let t = s.subrange(1, s.len() as int);
        lemma_trim_start(t);
        let k = choose|k: int| 0 <= k <= t.len() && trim_start(t) == t.subrange(k, t.len() as int);
        assert(t.subrange(k, t.len() as int) =~= s.subrange(k + 1, s.len() as int));
    } else {
        assert(s.subrange(0, s.len() as int) =~= s);
    }
}

proof fn lemma_trim_end(s: Seq<char>)
    ensures
        exists|k: int| 0 <= k <= s.len() && trim_end(s) == s.subrange(0, k),
        trim_end(s).len() > 0 ==> !trimmed_char(trim_end(s).last()) && trim_end(s)[0] == s[0],
    decreases s.len(),
{
    if s.len() > 0 && trimmed_char(s.last()) {
        let t = s.drop_last();
        lemma_trim_end(t);
        let k = choose|k: int| 0 <= k <= t.len() && trim_end(t) == t.subrange(0, k);
        assert(t.subrange(0, k) =~= s.subrange(0, k));
    } else {
        assert(s.subrange(0, s.len() as int) =~= s);
    }
}

proof fn lemma_trim_end_dots(s: Seq<char>)
    requires
        s.len() > 0,
        s[0] != '.',
    ensures
        exists|k: int| 1 <= k <= s.len() && trim_end_dots(s) == s.subrange(0, k),
        trim_end_dots(s).len() > 0,
        trim_end_dots(s).last() != '.',
    decreases s.len(),
{
    if s.last() == '.' {
        let t = s.drop_last();
        assert(s.len() > 1);
        assert(t[0] == s[0]);
        lemma_trim_end_dots(t);
        let k = choose|k: int| 1 <= k <= t.len() && trim_end_dots(t) == t.subrange(0, k);
        assert(t.subrange(0, k) =~= s.subrange(0, k));
    } else {
        assert(s.subrange(0, s.len() as int) =~= s);
    }
}

/// What cleaned() guarantees.
proof fn lemma_cleaned(raw: Seq<char>)
    ensures
        all_ok(cleaned(raw)),
        no_dd(cleaned(raw)),
        cleaned(raw).len() > 0 ==> !trimmed_char(cleaned(raw)[0]) && !trimmed_char(cleaned(raw).last()),
{
    let m = step_map(raw);
    lemma_map_ok(raw);
    let r = replace_dd(m);
    lemma_replace_dd(m);
    let a = trim_start(r);
    lemma_trim_start(r);
    let k = choose|k: int| 0 <= k <= r.len() && a == r.subrange(k, r.len() as int);
    lemma_subrange_props(r, k, r.len() as int);
    lemma_trim_end(a);
    let j = choose|j: int| 0 <= j <= a.len() && trim_end(a) == a.subrange(0, j);
    lemma_subrange_props(a, 0, j);
}

proof fn lemma_blen_concat(a: Seq<char>, b: Seq<char>)
    ensures
        blen(a + b) == blen(a) + blen(b),
    decreases b.len(),
{
    if b.len() > 0 {
        lemma_blen_concat(a, b.drop_last());
        assert((a + b).drop_last() =~= a + b.drop_last());
        assert((a + b).last() == b.last());
    } else {
        assert(a + b =~= a);
    }
}

proof fn lemma_blen_bounds(s: Seq<char>)
    ensures
        s.len() <= blen(s) <= 4 * s.len(),
    decreases s.len(),
{
    if s.len() > 0 {
        lemma_blen_bounds(s.drop_last());
    }
}

proof fn lemma_blen_prefix(s: Seq<char>, k: int)
    requires
        0 <= k <= s.len(),
    ensures
        blen(s.subrange(0, k)) <= blen(s),
{
    let p = s.subrange(0, k);
    let q = s.subrange(k, s.len() as int);
    assert(p + q =~= s);
    lemma_blen_concat(p, q);
}

proof fn lemma_blen_ascii(s: Seq<char>)
    requires
        forall|i: int| 0 <= i < s.len() ==> (#[trigger] s[i] as u32) < 0x80,
    ensures
        blen(s) == s.len(),
    decreases s.len(),
{
    if s.len() > 0 {
        let d = s.drop_last();
        assert forall|i: int| 0 <= i < d.len() implies (#[trigger] d[i] as u32) < 0x80 by {
            assert(d[i] == s[i]);
        }
        lemma_blen_ascii(d);
        assert(s.last() == s[s.len() - 1]);
    }
}

proof fn lemma_pop_to(s: Seq<char>, lim: int)
    requires
        lim >= 4,
    ensures
        exists|k: int| 0 <= k <= s.len() && pop_to(s, lim) == s.subrange(0, k),
        blen(pop_to(s, lim)) <= lim,
        s.len() > 0 ==> pop_to(s, lim).len() > 0,
    decreases s.len(),
{
    if s.len() > 0 && blen(s) > lim {
        let d = s.drop_last();
        if d.len() == 0 {
            // one char is at most 4 bytes
            assert(blen(s) == blen(d) + utf8_len(s.last()));
            assert(false);
        }
        lemma_pop_to(d, lim);
        let k = choose|k: int| 0 <= k <= d.len() && pop_to(d, lim) == d.subrange(0, k);
        assert(d.subrange(0, k) =~= s.subrange(0, k));
    } else {
        assert(s.subrange(0, s.len() as int) =~= s);
    }
}

proof fn lemma_last_dot(s: Seq<char>)
    ensures
        -1 <= last_dot(s) < s.len(),
        last_dot(s) >= 0 ==> s[last_dot(s)] == '.',
        forall|j: int| last_dot(s) < j < s.len() ==> #[trigger] s[j] != '.',
    decreases s.len(),
{
    if s.len() > 0 && s.last() != '.' {
        let d = s.drop_last();
        lemma_last_dot(d);
        assert forall|j: int| last_dot(s) < j < s.len() implies #[trigger] s[j] != '.' by {
            if j < s.len() - 1 {
                assert(s[j] == d[j]);
            }
        }
    }
}

/// The suffix: empty, or '.' followed by dot-free chars from s; at most 21 bytes.
proof fn lemma_ext_suffix(s: Seq<char>)
    requires
        all_ok(s),
    ensures
        ({
            let e = ext_suffix(s);
            &&& blen(e) <= 21
            &&& all_ok(e)
            &&& (e.len() > 0 ==> e[0] == '.')
            &&& forall|i: int| 1 <= i < e.len() ==> #[trigger] e[i] != '.'
        }),
{
    lemma_last_dot(s);
    let e = ext_suffix(s);
    let d = last_dot(s);
    if d > 0 {
        let x = s.subrange(d + 1, s.len() as int);
        lemma_subrange_props(s, d + 1, s.len() as int);
        if blen(x) <= 20 {
            assert(e == seq!['.'] + x);
            lemma_blen_concat(seq!['.'], x);
            let dot = seq!['.'];
            assert forall|i: int| 0 <= i < dot.len() implies (#[trigger] dot[i] as u32) < 0x80 by {}
            lemma_blen_ascii(dot);
            assert forall|i: int| 0 <= i < e.len() implies char_ok(#[trigger] e[i]) by {
                if i > 0 {
                    assert(e[i] == x[i - 1]);
                }
            }
            assert forall|i: int| 1 <= i < e.len() implies #[trigger] e[i] != '.' by {
                assert(e[i] == x[i - 1]);
                assert(x[i - 1] == s[d + i]);
            }
        }
    }
}

proof fn lemma_concat_props(a: Seq<char>, e: Seq<char>)
    requires
        all_ok(a),
        all_ok(e),
        no_dd(a),
        e.len() > 0 ==> e[0] == '.',
        forall|i: int| 1 <= i < e.len() ==> #[trigger] e[i] != '.',
    ensures
        all_ok(a + e),
        (a.len() == 0 || a.last() != '.') ==> no_dd(a + e),
{
    let o = a + e;
    assert forall|i: int| 0 <= i < o.len() implies char_ok(#[trigger] o[i]) by {
        if i < a.len() {
            assert(o[i] == a[i]);
        } else {
            assert(o[i] == e[i - a.len()]);
        }
    }
    if a.len() == 0 || a.last() != '.' {
        assert forall|i: int| 0 <= i < o.len() - 1 implies !(#[trigger] o[i] == '.' && o[i + 1] == '.') by {
            if i + 1 < a.len() {
                assert(o[i] == a[i]);
                assert(o[i + 1] == a[i + 1]);
            } else if i + 1 == a.len() {
                assert(o[i] == a.last());
            } else {
                assert(o[i + 1] == e[i + 1 - a.len()]);
            }
        }
    }
}

proof fn lemma_file_name()
    ensures
        all_ok(file_name()),
        no_dd(file_name()),
        blen(file_name()) == 4,
{
    let f = file_name();
    assert forall|i: int| 0 <= i < f.len() implies (#[trigger] f[i] as u32) < 0x80 && char_ok(f[i]) by {}
    lemma_blen_ascii(f);
}

/// The common part of SK4.4: the long-name branch before the extension is pushed.
proof fn lemma_cut(s: Seq<char>)
    requires
        s.len() > 0,
        blen(s) > 120,
        all_ok(s),
        no_dd(s),
        !trimmed_char(s[0]),
    ensures
        ({
            let e = ext_suffix(s);
            let lim = 120 - blen(e);
            let cut = pop_to(clip(s, lim), lim);
            &&& cut.len() > 0 && cut[0] == s[0]
            &&& blen(cut) + blen(e) <= 120
            &&& all_ok(cut) && no_dd(cut)
        }),
{
    lemma_ext_suffix(s);
    let e = ext_suffix(s);
    let lim = 120 - blen(e);
    let c = clip(s, lim);
    assert(c.len() > 0 && c == s.subrange(0, c.len() as int)) by {
        if 0 <= lim < s.len() {
            assert(s.take(lim) =~= s.subrange(0, lim));
        } else {
            assert(s =~= s.subrange(0, s.len() as int));
        }
    }
    lemma_subrange_props(s, 0, c.len() as int);
    lemma_pop_to(c, lim);
    let k = choose|k: int| 0 <= k <= c.len() && pop_to(c, lim) == c.subrange(0, k);
    lemma_subrange_props(c, 0, k);
}

/// SK4.4: the fixed sanitize is safe.
pub proof fn sanitize_fixed_safe(raw: Seq<char>)
    ensures
        ({
            let o = sanitize(raw, true);
            &&& o.len() > 0
            &&& blen(o) <= 120
            &&& all_ok(o)
            &&& no_dd(o)
        }),
{
    lemma_cleaned(raw);
    lemma_file_name();
    let s = cleaned(raw);
    if s.len() > 0 && blen(s) > 120 {
        lemma_cut(s);
        lemma_ext_suffix(s);
        let e = ext_suffix(s);
        let lim = 120 - blen(e);
        let cut = pop_to(clip(s, lim), lim);
        assert(cut[0] != '.');
        lemma_trim_end_dots(cut);
        let t = trim_end_dots(cut);
        let k = choose|k: int| 1 <= k <= cut.len() && t == cut.subrange(0, k);
        lemma_subrange_props(cut, 0, k);
        lemma_blen_prefix(cut, k);
        lemma_concat_props(t, e);
        lemma_blen_concat(t, e);
    }
}

/// SK4.4: today's sanitize: everything except "no ..".
pub proof fn sanitize_safe_but_dotdot(raw: Seq<char>)
    ensures
        ({
            let o = sanitize(raw, false);
            &&& o.len() > 0
            &&& blen(o) <= 120
            &&& all_ok(o)
        }),
{
    lemma_cleaned(raw);
    lemma_file_name();
    let s = cleaned(raw);
    if s.len() > 0 && blen(s) > 120 {
        lemma_cut(s);
        lemma_ext_suffix(s);
        let e = ext_suffix(s);
        let lim = 120 - blen(e);
        let cut = pop_to(clip(s, lim), lim);
        lemma_concat_props(cut, e);
        lemma_blen_concat(cut, e);
    }
}

/// "a"*115 + "." + "b"*10 + ".jpg"
pub open spec fn dotdot_witness() -> Seq<char> {
    Seq::new(
        130,
        |i: int|
            if i == 115 || i == 126 {
                '.'
            } else if i == 127 {
                'j'
            } else if i == 128 {
                'p'
            } else if i == 129 {
                'g'
            } else if i > 115 {
                'b'
            } else {
                'a'
            },
    )
}

/// The bug: today's sanitize turns the witness into "a"*115 + "..jpg".
pub proof fn unfixed_sanitize_keeps_dotdot()
    ensures
        !no_dd(sanitize(dotdot_witness(), false)),
        no_dd(sanitize(dotdot_witness(), true)),
        sanitize(dotdot_witness(), false).len() == 120,
{
    let w = dotdot_witness();
    assert forall|i: int| 0 <= i < w.len() implies (#[trigger] w[i] as u32) < 0x80 && char_ok(w[i]) by {}
    // map: identity
    assert(step_map(w) =~= w) by {
        assert forall|i: int| 0 <= i < w.len() implies #[trigger] step_map(w)[i] == w[i] by {
            assert(char_ok(w[i]));
        }
    }
    // replace(".."): identity
    assert(no_dd(w)) by {
        assert forall|i: int| 0 <= i < w.len() - 1 implies !(#[trigger] w[i] == '.' && w[i + 1] == '.') by {}
    }
    lemma_no_dd_fixpoint(w);
    // trim: identity
    assert(trim_start(w) == w);
    assert(w.last() == 'g');
    assert(trim_end(w) == w);
    assert(cleaned(w) == w);
    lemma_blen_ascii(w);
    assert(blen(w) == 130);
    // extension: "jpg"
    let d1 = w.drop_last();
    let d2 = d1.drop_last();
    let d3 = d2.drop_last();
    assert(d1.last() == 'p');
    assert(d2.last() == 'j');
    assert(d3.last() == '.');
    assert(d3.len() == 127);
    assert(last_dot(d3) == 126);
    assert(last_dot(d2) == last_dot(d3));
    assert(last_dot(d1) == last_dot(d2));
    assert(last_dot(w) == last_dot(d1));
    let x = w.subrange(127, 130);
    assert forall|i: int| 0 <= i < x.len() implies (#[trigger] x[i] as u32) < 0x80 by {
        assert(x[i] == w[127 + i]);
    }
    lemma_blen_ascii(x);
    let e = seq!['.'] + x;
    assert(ext_suffix(w) == e);
    assert forall|i: int| 0 <= i < e.len() implies (#[trigger] e[i] as u32) < 0x80 by {
        if i > 0 {
            assert(e[i] == x[i - 1]);
        }
    }
    lemma_blen_ascii(e);
    assert(blen(e) == 4);
    // the cut: 116 ASCII chars, nothing popped
    let c = w.take(116);
    assert(clip(w, 116) == c);
    assert forall|i: int| 0 <= i < c.len() implies (#[trigger] c[i] as u32) < 0x80 by {
        assert(c[i] == w[i]);
    }
    lemma_blen_ascii(c);
    assert(pop_to(c, 116) == c);
    let o = c + e;
    assert(sanitize(w, false) == o);
    assert(o[115] == '.');
    assert(o[116] == '.');
    sanitize_fixed_safe(w);
}

// ================================================================ SK1.6-SK1.7: voice notes
// Branch voice-notes (438d748): router::route_for, router::timeline_kind_for, the attachment
// kind receive stores (mod.rs::receive) and the handler extract::read picks.

/// router::route_for. `container` <-> head[4..8] == "ftyp" || head starts with 1A 45 DF A3
/// (MP4 / Matroska containers, which sniff calls video); `i.mime == Some(Audio)` <->
/// `mime_kind(mime) == Some("audio")`.
pub open spec fn route_for_spec(i: Input, voice: bool, container: bool) -> Word {
    if voice && route_spec(i) == Word::Video && i.mime == Some(Word::Audio) && container {
        Word::Audio
    } else {
        route_spec(i)
    }
}

pub fn route_for(i: &Input, voice: bool, container: bool) -> (r: Word)
    ensures
        r == route_for_spec(*i, voice, container),
{
    let k = route(i);
    if voice && matches!(k, Word::Video) && matches!(i.mime, Some(Word::Audio)) && container {
        Word::Audio
    } else {
        k
    }
}

/// router::timeline_kind_for: only audio may opt into voice-note rendering.
pub open spec fn timeline_kind_for_spec(w: Word, voice: bool) -> Word {
    if w == Word::Audio && voice {
        Word::Voice
    } else {
        timeline_kind_spec(w)
    }
}

pub fn timeline_kind_for(w: Word, voice: bool) -> (r: Word)
    ensures
        r == timeline_kind_for_spec(w, voice),
{
    if matches!(w, Word::Audio) && voice {
        Word::Voice
    } else {
        timeline_kind(w)
    }
}

/// The attachment kind receive stores for a file part whose `meta.files[i].voice` is `voice`
/// (a part without a meta entry gets `timeline_kind(route_for(.., false))`, the same value).
pub open spec fn received_kind(i: Input, voice: bool, container: bool) -> Word {
    timeline_kind_for_spec(route_for_spec(i, voice, container), voice)
}

/// SK1.6: the voice flag. It never makes the router partial or leave KINDS; it changes the
/// handler only for an audio-MIME MP4/Matroska container that sniff calls video (to audio);
/// a part gets timeline kind `voice` exactly when it is flagged and routed audio; a flagged
/// part that is not routed audio gets exactly the timeline kind of the same upload without
/// the flag; every result passes normalize's whitelist.
pub proof fn voice_routing(i: Input, voice: bool, container: bool)
    requires
        well_formed(i),
    ensures
        kinds().contains(route_for_spec(i, voice, container)),
        route_for_spec(i, false, container) == route_spec(i),
        route_for_spec(i, voice, container) != route_spec(i) ==> voice && route_spec(i)
            == Word::Video && i.mime == Some(Word::Audio) && container && route_for_spec(
            i,
            voice,
            container,
        ) == Word::Audio,
        received_kind(i, voice, container) == Word::Voice <==> voice && route_for_spec(
            i,
            voice,
            container,
        ) == Word::Audio,
        route_for_spec(i, voice, container) != Word::Audio ==> received_kind(i, voice, container)
            == received_kind(i, false, container) && received_kind(i, voice, container)
            == timeline_kind_spec(route_spec(i)),
        attachment_kinds().contains(received_kind(i, voice, container)),
        received_kind(i, false, container) == timeline_kind_spec(route_spec(i)),
{
    route_total_in_kinds(i);
    lemma_kinds_exactly(route_for_spec(i, voice, container));
    lemma_attachment_kinds_exactly(received_kind(i, voice, container));
}

/// SK1.6 for every string: timeline_kind_for is whitelisted, and the flag changes nothing
/// but audio.
pub proof fn timeline_kind_for_whitelisted(w: Word, voice: bool)
    ensures
        attachment_kinds().contains(timeline_kind_for_spec(w, voice)),
        w != Word::Audio ==> timeline_kind_for_spec(w, voice) == timeline_kind_spec(w),
        timeline_kind_for_spec(w, voice) == Word::Voice <==> w == Word::Audio && voice,
{
    lemma_attachment_kinds_exactly(timeline_kind_for_spec(w, voice));
}

/// extract::read: the handler (`route_for(.., kind == "voice")`) and whether `audio` runs with
/// `voice = true` (subtype "voice": the bare transcript becomes the message).
pub open spec fn extract_handler(i: Input, kind: Word, container: bool) -> Word {
    route_for_spec(i, kind == Word::Voice, container)
}

pub open spec fn voice_subtype(handler: Word, kind: Word) -> bool {
    handler == Word::Audio && kind == Word::Voice
}

/// SK1.7: receive and extract agree. extract re-routes with the flag read back from the
/// stored kind; it picks the handler receive picked, and a part stored as `voice` always
/// reaches the audio handler with subtype voice (so its transcript, or "couldn't hear it",
/// decides the reply), while no other part does.
pub proof fn receive_and_extract_agree(i: Input, voice: bool, container: bool)
    requires
        well_formed(i),
    ensures
        extract_handler(i, received_kind(i, voice, container), container) == route_for_spec(
            i,
            voice,
            container,
        ),
        voice_subtype(
            extract_handler(i, received_kind(i, voice, container), container),
            received_kind(i, voice, container),
        ) <==> received_kind(i, voice, container) == Word::Voice,
{
    voice_routing(i, voice, container);
}

// ================================================================ SK4.5: voice-note fields
// lucina-store/src/timeline.rs (branch voice-notes): valid_waveform (base64 0.22.1, STANDARD
// alphabet, padding Indifferent, trailing bits rejected), clean_string(.., MAX_TRANSCRIPT_CHARS)
// = clip_utf16(js_trim(..), 4000); share/mod.rs clip(.., 4000) (chars) at receive and in
// process. Strings are taken after the trim.

pub open spec fn b64_sym(c: u8) -> bool {
    (0x41 <= c <= 0x5a) || (0x61 <= c <= 0x7a) || (0x30 <= c <= 0x39) || c == 0x2b || c == 0x2f
}

pub open spec fn b64_val(c: u8) -> int {
    if 0x41 <= c <= 0x5a {
        c - 0x41
    } else if 0x61 <= c <= 0x7a {
        c - 0x61 + 26
    } else if 0x30 <= c <= 0x39 {
        c - 0x30 + 52
    } else if c == 0x2b {
        62
    } else {
        63
    }
}

/// The chunk base64's decode_suffix reads: the last n % 4 bytes, or the last 4.
pub open spec fn tail_len(n: nat) -> nat {
    if n == 0 {
        0
    } else if n % 4 == 0 {
        4
    } else {
        n % 4
    }
}

/// '=' at the end of the tail (at most two can be legal).
pub open spec fn tail_pads(t: Seq<u8>) -> nat {
    if t.len() >= 1 && t[t.len() - 1] == 0x3d {
        if t.len() >= 2 && t[t.len() - 2] == 0x3d {
            2
        } else {
            1
        }
    } else {
        0
    }
}

/// `ENGINE.decode(s).map(|b| b.len())`: Some(decoded length) or None (an error). The
/// complete quads before the tail are symbols only; the tail is k >= 2 symbols then '='
/// (padding optional: Indifferent); the bits after the last whole byte must be zero.
pub open spec fn b64_decoded_len_spec(s: Seq<u8>) -> Option<nat> {
    if s.len() == 0 {
        Some(0)
    } else {
        let n = s.len();
        let tl = tail_len(n);
        let p = n - tl;
        let t = s.subrange(p, n as int);
        let k = tl - tail_pads(t);
        if (forall|j: int| 0 <= j < p ==> b64_sym(#[trigger] s[j])) && k >= 2 && (forall|j: int|
            0 <= j < k ==> b64_sym(#[trigger] t[j])) && (k == 2 ==> b64_val(t[1]) % 16 == 0) && (k
            == 3 ==> b64_val(t[2]) % 4 == 0) {
            Some((3 * (p / 4) + (k - 1)) as nat)
        } else {
            None
        }
    }
}

pub fn b64_sym_exec(c: u8) -> (r: bool)
    ensures
        r == b64_sym(c),
{
    (0x41 <= c && c <= 0x5a) || (0x61 <= c && c <= 0x7a) || (0x30 <= c && c <= 0x39) || c == 0x2b
        || c == 0x2f
}

pub fn b64_val_exec(c: u8) -> (r: u8)
    requires
        b64_sym(c),
    ensures
        r as int == b64_val(c),
        r < 64,
{
    if 0x41 <= c && c <= 0x5a {
        c - 0x41
    } else if 0x61 <= c && c <= 0x7a {
        c - 0x61 + 26
    } else if 0x30 <= c && c <= 0x39 {
        c - 0x30 + 52
    } else if c == 0x2b {
        62
    } else {
        63
    }
}

pub fn b64_decoded_len(s: &[u8]) -> (r: Option<usize>)
    ensures
        r matches Some(x) ==> b64_decoded_len_spec(s@) == Some(x as nat),
        r is None ==> b64_decoded_len_spec(s@) is None,
{
    let n = s.len();
    if n == 0 {
        return Some(0);
    }
    let tl: usize = if n % 4 == 0 {
        4
    } else {
        n % 4
    };
    assert(tl == tail_len(n as nat));
    let p = n - tl;
    let ghost t = s@.subrange(p as int, n as int);
    let mut j: usize = 0;
    while j < p
        invariant
            j <= p,
            p == n - tl,
            n == s@.len(),
            n > 0,
            tl == tail_len(n as nat),
            forall|m: int| 0 <= m < j ==> b64_sym(#[trigger] s@[m]),
        decreases p - j,
    {
        if !b64_sym_exec(s[j]) {
            assert(!b64_sym(s@[j as int]));
            assert(!(forall|m: int| 0 <= m < p ==> b64_sym(#[trigger] s@[m])));
            assert(b64_decoded_len_spec(s@) is None);
            return None;
        }
        j += 1;
    }
    let pads: usize = if s[n - 1] == 0x3d {
        if tl >= 2 && s[n - 2] == 0x3d {
            2
        } else {
            1
        }
    } else {
        0
    };
    assert(t.len() == tl);
    assert(t[t.len() - 1] == s@[n - 1]);
    assert(tl >= 2 ==> t[t.len() - 2] == s@[n - 2]);
    assert(pads == tail_pads(t));
    if tl < pads + 2 {
        return None;
    }
    let k = tl - pads;
    let mut m: usize = 0;
    while m < k
        invariant
            m <= k,
            k == tl - pads,
            k <= tl,
            n > 0,
            tl == tail_len(n as nat),
            pads == tail_pads(t),
            p + tl == n,
            n == s@.len(),
            t == s@.subrange(p as int, n as int),
            forall|x: int| 0 <= x < m ==> b64_sym(#[trigger] t[x]),
        decreases k - m,
    {
        assert(t[m as int] == s@[p + m]);
        if !b64_sym_exec(s[p + m]) {
            assert(!b64_sym(t[m as int]));
            assert(!(forall|x: int| 0 <= x < k ==> b64_sym(#[trigger] t[x])));
            assert(b64_decoded_len_spec(s@) is None);
            return None;
        }
        m += 1;
    }
    if k == 2 {
        assert(t[1] == s@[p + 1]);
        if b64_val_exec(s[p + 1]) % 16 != 0 {
            return None;
        }
    }
    if k == 3 {
        assert(t[2] == s@[p + 2]);
        if b64_val_exec(s[p + 2]) % 4 != 0 {
            return None;
        }
    }
    Some(3 * (p / 4) + (k - 1))
}

/// timeline::valid_waveform on the trimmed string: Some iff this holds (then the trimmed
/// string is stored).
pub open spec fn valid_waveform_spec(s: Seq<u8>) -> bool {
    s.len() != 0 && s.len() <= 88 && (b64_decoded_len_spec(s) matches Some(d) && 1 <= d <= 64)
}

pub fn valid_waveform(s: &[u8]) -> (r: bool)
    ensures
        r == valid_waveform_spec(s@),
{
    if s.len() == 0 || s.len() > 88 {
        return false;
    }
    match b64_decoded_len(s) {
        Some(d) => 1 <= d && d <= 64,
        None => false,
    }
}

/// A string that decodes to at most 64 bytes has at most 88 characters (22 quads, the
/// last one padded); one that decodes to at least one byte is not empty.
pub proof fn lemma_decoded_len_bounds_chars(s: Seq<u8>)
    requires
        b64_decoded_len_spec(s) matches Some(d) && d <= 64,
    ensures
        s.len() <= 88,
        b64_decoded_len_spec(s) matches Some(d) && (d >= 1 ==> s.len() != 0),
{
    if s.len() != 0 {
        let n = s.len();
        let tl = tail_len(n);
        let p = n - tl;
        assert(p % 4 == 0 && 1 <= tl <= 4) by {
            assert(n == 4 * (n / 4) + n % 4);
        }
        assert(p == 4 * (p / 4));
    }
}

/// SK4.5: valid_waveform accepts exactly the (trimmed) strings that base64-decode to 1..=64
/// bytes: the 88-character pre-check never rejects one of them, and a stored waveform is
/// at most 88 bytes of base64 that decodes to 1..=64 levels.
pub proof fn valid_waveform_exactly(s: Seq<u8>)
    ensures
        valid_waveform_spec(s) <==> (b64_decoded_len_spec(s) matches Some(d) && 1 <= d <= 64),
        valid_waveform_spec(s) ==> 1 <= s.len() <= 88,
{
    if b64_decoded_len_spec(s) matches Some(d) && 1 <= d <= 64 {
        lemma_decoded_len_bounds_chars(s);
    }
}

pub open spec fn all_a(s: Seq<u8>) -> bool {
    forall|j: int| 0 <= j < s.len() ==> #[trigger] s[j] == 0x41
}

/// Witnesses: both waveform checks are needed and the 88 cap is tight. 88 'A's decode to
/// 66 bytes (the length cap alone would keep them); 86 'A's (unpadded) and 84 'A's + "AA=="
/// (padded, 88 chars) decode to exactly 64 bytes and are kept.
pub proof fn waveform_limits_witness()
    ensures
        ({
            let s = Seq::new(88, |j: int| 0x41u8);
            s.len() == 88 && b64_decoded_len_spec(s) == Some(66nat) && !valid_waveform_spec(s)
        }),
        ({
            let s = Seq::new(86, |j: int| 0x41u8);
            b64_decoded_len_spec(s) == Some(64nat) && valid_waveform_spec(s)
        }),
        ({
            let s = Seq::new(84, |j: int| 0x41u8) + seq![0x41u8, 0x41u8, 0x3du8, 0x3du8];
            s.len() == 88 && b64_decoded_len_spec(s) == Some(64nat) && valid_waveform_spec(s)
        }),
{
    let a = Seq::new(88, |j: int| 0x41u8);
    let ta = a.subrange(84, 88);
    assert(tail_len(88) == 4);
    assert(ta[3] == 0x41u8);
    assert(tail_pads(ta) == 0);
    assert(forall|j: int| 0 <= j < 84 ==> b64_sym(#[trigger] a[j]));
    assert(forall|j: int| 0 <= j < 4 ==> b64_sym(#[trigger] ta[j]));
    assert(b64_decoded_len_spec(a) == Some(66nat));
    let b = Seq::new(86, |j: int| 0x41u8);
    let tb = b.subrange(84, 86);
    assert(tail_len(86) == 2);
    assert(tb[1] == 0x41u8);
    assert(tail_pads(tb) == 0);
    assert(forall|j: int| 0 <= j < 84 ==> b64_sym(#[trigger] b[j]));
    assert(forall|j: int| 0 <= j < 2 ==> b64_sym(#[trigger] tb[j]));
    assert(b64_val(tb[1]) == 0);
    assert(b64_decoded_len_spec(b) == Some(64nat));
    let c = Seq::new(84, |j: int| 0x41u8) + seq![0x41u8, 0x41u8, 0x3du8, 0x3du8];
    let tc = c.subrange(84, 88);
    assert(c.len() == 88);
    assert(tail_len(88) == 4);
    assert(tc =~= seq![0x41u8, 0x41u8, 0x3du8, 0x3du8]);
    assert(tail_pads(tc) == 2);
    assert(forall|j: int| 0 <= j < 84 ==> b64_sym(#[trigger] c[j])) by {
        assert forall|j: int| 0 <= j < 84 implies b64_sym(#[trigger] c[j]) by {
            assert(c[j] == 0x41u8);
        }
    }
    assert(forall|j: int| 0 <= j < 2 ==> b64_sym(#[trigger] tc[j]));
    assert(b64_val(tc[1]) == 0);
    assert(b64_decoded_len_spec(c) == Some(64nat));
}

/// UTF-16 code units of a char (`char::len_utf16`).
pub open spec fn u16len(c: char) -> nat {
    if (c as u32) >= 0x10000 {
        2
    } else {
        1
    }
}

pub open spec fn units(s: Seq<char>) -> nat
    decreases s.len(),
{
    if s.len() == 0 {
        0
    } else {
        u16len(s[0]) + units(s.drop_first())
    }
}

/// timeutil::clip_utf16: the longest prefix of whole chars within `max` UTF-16 units.
pub open spec fn clip16(s: Seq<char>, max: nat) -> Seq<char>
    decreases s.len(),
{
    if s.len() == 0 || u16len(s[0]) > max {
        Seq::empty()
    } else {
        seq![s[0]] + clip16(s.drop_first(), (max - u16len(s[0])) as nat)
    }
}

/// A transcript as the timeline stores it (attachment or meta): clean_string(.., 4000).
pub open spec fn stored_transcript(s: Seq<char>) -> Option<Seq<char>> {
    let c = clip16(s, 4000);
    if c.len() == 0 {
        None
    } else {
        Some(c)
    }
}

proof fn lemma_clip16(s: Seq<char>, max: nat)
    ensures
        units(clip16(s, max)) <= max,
        clip16(s, max).len() <= max,
        clip16(s, max).len() <= s.len(),
        clip16(s, max) == s.take(clip16(s, max).len() as int),
    decreases s.len(),
{
    if s.len() > 0 && u16len(s[0]) <= max {
        let r = clip16(s.drop_first(), (max - u16len(s[0])) as nat);
        lemma_clip16(s.drop_first(), (max - u16len(s[0])) as nat);
        let c = seq![s[0]] + r;
        assert(c.drop_first() =~= r);
        assert(c[0] == s[0]);
        assert(c =~= s.take(c.len() as int)) by {
            assert forall|j: int| 0 <= j < c.len() implies #[trigger] c[j] == s.take(c.len() as int)[j] by {
                if j > 0 {
                    assert(c[j] == r[j - 1]);
                    assert(r[j - 1] == s.drop_first().take(r.len() as int)[j - 1]);
                }
            }
        }
    } else {
        assert(clip16(s, max) =~= s.take(0));
    }
}

/// receive's and process's `clip(.., n)` (chars) before normalize's clip16 to m <= n units
/// never changes what clip16 keeps.
proof fn lemma_clip16_of_take(s: Seq<char>, n: nat, m: nat)
    requires
        m <= n,
    ensures
        clip16(clip(s, n as int), m) == clip16(s, m),
    decreases s.len(),
{
    if n < s.len() {
        lemma_clip16_take(s, n, m);
    }
}

proof fn lemma_clip16_take(s: Seq<char>, n: nat, m: nat)
    requires
        m <= n,
        n <= s.len(),
    ensures
        clip16(s.take(n as int), m) == clip16(s, m),
    decreases s.len(),
{
    let t = s.take(n as int);
    if n == 0 {
        assert(t.len() == 0);
        assert(m == 0);
        if s.len() > 0 {
            assert(u16len(s[0]) >= 1);
        }
    } else {
        assert(t[0] == s[0]);
        if u16len(s[0]) <= m {
            let m2 = (m - u16len(s[0])) as nat;
            assert(t.drop_first() =~= s.drop_first().take((n - 1) as int));
            lemma_clip16_take(s.drop_first(), (n - 1) as nat, m2);
        }
    }
}

/// SK4.5: transcripts. What the timeline stores on a voice attachment (`transcript`) and on
/// Lucina's reply (`meta.transcript`) is non-empty and at most 4000 UTF-16 units, so at most
/// 4000 chars; and the 4000-char clip that receive/process apply first (the copy in
/// status.json) never cuts what clip16 keeps (normalize trims between the two clips, which
/// can drop whitespace left at the cut; that trim is outside this lemma).
pub proof fn voice_transcript_bounded(s: Seq<char>)
    ensures
        stored_transcript(s) matches Some(c) ==> 1 <= c.len() <= 4000 && units(c) <= 4000
            && c == s.take(c.len() as int),
        stored_transcript(clip(s, 4000)) == stored_transcript(s),
{
    lemma_clip16(s, 4000);
    lemma_clip16_of_take(s, 4000, 4000);
}

} // verus!
fn main() {}
