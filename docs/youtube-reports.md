# YouTube video reports

When the owner shares a YouTube link and asks for a transcript, a report or a summary
("Transcribe this video and visual inspection with awkward terms then do a report"), Lucina
answers at once and runs the job `youtube_report` in the background. When it is done she
replies in the same chat with the summary and a link to a Keep note labelled **YouTube**, and
the owner's devices get a notification.

Code: `crates/lucina-server/src/video/` (pipeline) and `src/chat/youtube_turn.rs` (trigger).

## Trigger

`chat::json_turn` calls `youtube_turn::try_turn` before anything else, so it covers every
channel that reaches a chat turn: iOS shares (`/v1/share`), web chat, Telegram text and
trusted MCP. Conditions (`video::trigger`):

- the owner's turn (never a guarded/non-owner one);
- a YouTube video URL in the message (watch, shorts, youtu.be, live, embed), or a share's
  `"video_id":"…"` data;
- the owner's own words (for a share, only the note, not the shared description) ask for a
  transcript/report/summary/analysis (en, it, da).

A video that already has a report gets the existing note back; "again"/"redo" makes a new one.
A second request while the job runs gets "already working on it". One video job runs at a time.

Progress: the job record (`GET /api/secretary/jobs/{id}`) carries `stage` and `stageAt`.

## Pipeline

| Step | Module | What |
| --- | --- | --- |
| Fetch | `fetch` | yt-dlp, pinned (`LUCINA_YTDLP_VERSION`, refuses another version), `--js-runtimes node`; best video+audio → `video.mkv`, trimmed `info.json`, `description.txt`, `chapters.json`, `thumbnail.jpg`; subtitles in a separate best-effort call (YouTube answers 429 to subtitle requests often) |
| Archive | `archive` | `LUCINA_VIDEOS_DIR` (MyBook `lucina-videos/<YYYY>/<YYYY-MM-DD> <title> [<id>]/`), `index.json` id → folder + note (dedup) |
| Library | `fetch::register_media` | lucina-media `PUT /internal/v1/external/youtube/<id>`, album "YouTube" |
| Whisper | `transcript` | lucina-memo's whisper.cpp path (GPU lease first, CPU model fallback), primed with `--prompt` from the title, tags and chapters so jargon is spelled right |
| Visual | `visual` | scene-change + sampled keyframes (iGPU VAAPI decode when it works), tesseract OCR, Gemini vision in batches (local Gemma fallback) → `keyframes/`, `visual.json` |
| ZCode | `zcode` | ≤ 8 MB MP4 chunk(s) for Z.ai's vision MCP video tool, ZCode CLI (`node zcode.cjs … -p`) in a capped scope → `zcode/` |
| Merge | `merge` | 20 s windows; where Whisper, ZCode and captions disagree Gemini keeps the better text (or fixes a misheard term); otherwise Whisper |
| Report | `report` | Gemini (then Lucina's DeepSeek) writes summary, key points, glossary, on-screen code, visual notes as JSON; Markdown with timestamp links; Keep note `youtube-<id>` labelled YouTube; `report.md` + `transcript.md` in the folder |

Steps 2–4 run concurrently. A step that fails is listed under "How this was made" in the note
instead of failing the job; only fetch, archive and saving the note are fatal.

## Host rules

- CPU-heavy subprocesses (ffmpeg, tesseract, the ZCode node process) run through
  `video::thermal::command`: a transient `systemd-run --user --scope` with `CPUQuota`
  (`LUCINA_VIDEO_CPU_QUOTA`, 400%), `MemoryMax` (`LUCINA_VIDEO_MEMORY_MAX`, 6G) and nice 15. This
  also keeps them out of lucina-rs.service's 768M cap.
- Before a heavy step the job waits while Tctl ≥ `LUCINA_VIDEO_MAX_TCTL` (90 °C), at most
  `LUCINA_VIDEO_COOL_WAIT_SEC` (20 min), then goes on at low priority.
- Gemini uses Lucina's own `GEMINI_API_KEY` (gemini.env drop-in). Never the paid DeepSeek key
  through DSH/flash-do/lane; the report's fallback is Lucina's own model.

## Settings (env)

`LUCINA_YOUTUBE_REPORTS` (on), `LUCINA_VIDEOS_DIR`, `LUCINA_VIDEO_WORK_DIR`, `LUCINA_YTDLP`,
`LUCINA_YTDLP_VERSION`, `LUCINA_YTDLP_JS_RUNTIME`, `LUCINA_VIDEO_MAX_SEC` (3 h),
`LUCINA_VIDEO_GEMINI_MODEL` (gemini-3.8-flash), `LUCINA_VIDEO_GEMINI_FALLBACK_MODEL`
(gemini-3.5-flash), `LUCINA_VIDEO_ZCODE` (on), `LUCINA_ZCODE_DIR`, `LUCINA_VIDEO_ZCODE_TIMEOUT_SEC`,
`LUCINA_VIDEO_WHISPER_TIMEOUT_SEC`, `LUCINA_VIDEO_GPU_WAIT_SEC`, plus the thermal ones above.

## Updating yt-dlp

YouTube breaks old yt-dlp versions. Upgrade (`pipx upgrade yt-dlp`), run a fetch by hand, then
set `LUCINA_YTDLP_VERSION` in `~/.config/lucina-rs/mode.env` to the new version and restart
lucina-rs. Until the pin matches, jobs fail with "yt-dlp is X but Lucina is pinned to Y".
