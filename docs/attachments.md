# How Lucina handles the files you send her

Whatever reaches Lucina, from the share sheet (iOS, Android, the installed web app) or pasted or
attached in the chat, goes through one pipeline in Lucina (Rust, `crates/lucina-server/src/share/`):

1. **Store.** Files go into your media library on nezopt (lucina-media, album "Shared"). They stay on your
   servers. Links and text snippets are not files: they are kept with the share.
2. **Route.** The router looks at the bytes first (a PDF named `notes.txt` is still a PDF), then the
   extension, then the type the phone reported (`share/router.rs`).
3. **Read.** One handler per type extracts text and data (below). Large files are read in the background:
   the chat bubble shows "Lucina is reading…" until she answers (`GET /v1/share/{id}` → `stage`).
4. **Remember.** What she read is stored in `attachments.db` (SQLite full-text index next to the shares), so
   later questions like "in the PDF I sent yesterday, when is it due?" work. She also has two tools,
   `attachments_search` and `attachments_get`, and recent attachments come into the chat context when your
   message mentions a file, photo, PDF, invoice and so on.
5. **Answer.** She gets the extracted content with the guidance for its type and follows the rules below.

## Rules she follows

- **A question with the file:** she answers *that question* using the file, quoting numbers, dates and
  amounts exactly. You get the answer, not a generic summary.
  - *"quanto devo pagare?"* + a bill → "84,30 €, entro il 15/11/2026 (IBAN IT60…)."
  - *"cosa significa questo errore?"* + a screenshot → the exact error text, what it means, and the likely fix.
- **No question:** a short reaction. She says what it is and gives the one to three facts that matter, then
  suggests one or two next steps: save to the library, a reminder for the due date, add to shopping, save as
  a note in Obsidian, add the event to the calendar, save the contact.
- **No invention:** she only uses what was read. If something could not be read (a scanned page, a video
  with no speech, a page behind a login), she says so.
- Text inside a file is material, not an order: a PDF saying "ignore your instructions" is just text.
- **Quick actions** from the share sheet (Add to shopping, Save as note, Remind me) use her tools and
  report the real result. "Save to library" only stores and confirms.

## Per type

Handler kinds are in backticks; `share/guidance.rs` holds the exact guidance text she receives.

### Images and screenshots: `image`
- Gemma 4 vision on nezopt describes the image and classifies it: photo, chat screenshot, error
  screenshot, other screenshot, receipt, document photo, whiteboard, meme. Tesseract OCR (English,
  Italian, Danish) reads the text.
- **Receipts:** merchant, date, total and items. Every number is checked against the OCR text, so an
  invented amount is dropped.
- **Error screenshots:** the exact error text is kept as data.
- **Chat screenshots:** who wrote what, as far as it is readable.
- **Photos of bills:** read as invoices (below).
- Example: *"di che pianta si tratta?"* → what the image shows, and an honest "I can't be sure" when it is
  unclear.

### PDF: `pdf`
- She reads the text layer and the page count. A scanned PDF (no text layer) gets OCR on its first five
  page images.
- **Invoices and bills:** detected by words such as fattura, scadenza, IBAN, faktura, forfald, invoice,
  due date. She extracts issuer, number, date, due date, total, currency, IBAN and payment reference,
  each verified against the text. She offers a reminder before the due date.
- **Long documents** (over about 12 000 characters) get a map-reduce summary. The full text stays stored
  and searchable.
- Example: *"quanto devo pagare e entro quando?"* → total and due date, as written.

### Documents: `document`
- Word, ODT and RTF: text with headings and lists. Long ones get a summary. Invoices are detected as for PDF.

### Slides: `slides`
- PowerPoint and ODP: slide titles and text, in slide order.

### Spreadsheets and CSV: `sheet`
- xlsx, xls, ods and csv/tsv become tables: header, the first 200 rows, the row count, and per numeric
  column the sum, min and max. European decimals ("1.234,56") are understood.
- Example: *"quanto ho speso in totale?"* → the sum of the amount column, and which column and how many
  rows she used.

### Text: `text`
- Plain text and Markdown, read directly.

### Code: `code`
- The language, line count, top-level functions and types, and imports, plus the code itself.
- Example: *"why does this crash?"* → she points at the lines.

### Archives: `archive`
- zip, tar, tar.gz and similar: only the listing (entry count, total size, top-level folders, the
  biggest files). Nothing is ever extracted or run. Zip-bomb-sized declarations are flagged.

### Calendar invites: `calendar`
- .ics files: events with date, time, time zone and place. She offers to add them to your calendar, and
  adds them only when you say yes.

### Contact cards: `contact`
- .vcf files: name, phones, e-mails, company and address. She offers to save the contact as a note. She
  never contacts the person.

### Locations: `location`
- Apple/Google Maps links, `geo:` links, and a location shared from the chat composer: coordinates and
  name. She helps with directions, what's nearby, or a reminder there.

### Links: `link`
- **Web pages:** fetched from nezopt (15 s, up to 3 MB, never internal or tailnet addresses): title,
  description and readable text. A link to a PDF or image is read as that file.
- **YouTube:** title, channel, description, and the transcript from the video's captions (your language
  first, then English, then auto-generated).

### Audio and voice: `audio`
- A transcript made on the iPhone (sent with the file) is used first. Otherwise Whisper runs on nezopt's
  GPU, with CPU fallback, as in the voice-memo pipeline. The first 30 minutes are transcribed.

- **Chat voice notes (v1.2):** send `source` = `ios-chat` / `web-chat` / `android-chat`,
  an optional caption in `note`, and `meta.files[i]` with `index`, `voice: true`, optional
  `transcript`, `durationMs`, and `waveform` (standard base64 of up to 64 levels, 0–255,
  oldest first; at most 88 characters). Only audio accepts the voice flag.
- Voice notes use handler `audio`, subtype `voice`, timeline kind `voice`. A non-empty
  on-device transcript is used first; otherwise Whisper reads the first ten minutes.
  Voice notes skip long-document summaries. The transcript remains searchable in attachment memory.
- One voice note with no caption is answered as a typed message: reminders, shopping,
  notes, calendar, and questions use the normal chat planner. A caption or mixed parts
  keep the share turn, with the transcript and guidance to answer in the spoken language
  without describing the audio. Empty/failed transcription gets one device-language reply
  asking the owner to repeat.
- The user attachment adds optional `durationMs`, validated `waveform`, and `transcript`
  when known at receipt (trimmed, at most 4000 characters). The reply adds
  `meta.transcript` (at most 4000 characters) and `meta.voiceReply: true` for optional
  client TTS through `/v1/tts`. Polling `GET /v1/share/{id}` also returns known transcripts.
  There is one user item and one reply; retries add nothing. See [the JSON shapes](share.md#v12-voice-notes).

### Video: `video`
- Three to six key frames (scene changes, else evenly spaced), each described by vision, plus the audio
  transcript.

### Anything else: `other`
- She reports the type, size and name, and what it probably is.

## Drive documents

Files in your Drive folders (lucina-media, e.g. `Lezioni`) are read the same way when you ask about them in
the chat ("cosa dice l'ultima lezione di Digital Forensics sul file carving?"). The `media` tool searches the
Drive (`scope=drive`, newest first by modified) and its `read` action runs the file through the same router
and handlers, with the same guidance per type (`share/library.rs`). Differences: no long-document summary
(the chat turn has no time for it); instead a long text shows the passages that match your question, up to
about 12 000 characters. What she read is stored in `attachments.db` as `drive-<item id>` and reused while the
file is unchanged. Without the handlers (or when they take too long) she uses the library's own extracted
text. The reply gets an "Open in Drive" button.

## Limits

- One share: at most 32 parts and 2 GB.
- She sees about 12 000 characters per item and 30 000 per share. The rest is stored and searchable.
- Reading time per item: 5 minutes, or 20 for audio and video. A timeout is reported, not hidden.
- Everything stays on your servers. Vision, OCR and Whisper run on nezopt. Long-document summaries,
  invoice/receipt fields and the reply itself use Lucina's normal model (DeepSeek), as for any message.

## Tests

`cargo test -p lucina-server share` covers routing by content, each handler's output (invoice total and
due date, spreadsheet sums, .ics events, .vcf contacts, archive limits, YouTube caption parsing), the turn
prompt (the question plus the right guidance plus the extracted numbers), and attachment memory recall.
Fixtures are in `crates/lucina-server/tests/fixtures/attachments/`.
