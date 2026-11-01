# Voice Learn and the Lucina Voice thread (lucina-rs half)

docs/lucinavoice-protocol.md sections 7 and 8. Code: `crates/lucina-server/src/voice_learn.rs`,
`voice_thread.rs`, `chat.rs` (`handle_chat`), `crates/lucina-store/src/timeline.rs`.

## The thread

- Channel `voice` joins `lucina_store::timeline::CHANNELS`; the conversation key rule maps it to `voice`, titled
  "Lucina Voice", one conversation for every speaker (`device` and `deviceName` stay on each item). The chat route
  accepts `channel: "voice"` (validation in `chat.rs`) and defaults the session to `lucinavoice` when none is given.
- Memory surface: the kernel in `lucina-memory` knows only the app channels, so `surface::chat_surface` classifies a
  voice turn like an iOS turn (owner device behind the Secretary bearer). The timeline channel stays `voice`.
- `meta.voice` on both items of a turn: `{turnId, ms:{..}, route:{intent, fast, source, confidence, jevMs, layers}}`.
  `turnId` and `ms` come from the request's `voiceMeta` (secretary-server: `{turnId, ms:{stt}, ambiguous?}`);
  lucina-rs adds `route` and the layers, which it alone knows. The owner item is written when the route is known
  (at most one Jev cap after the request), the Lucina item after the reply. End-to-end spans (`done.ms` of the
  device socket) are unknown here: secretary-server puts what it has in `voiceMeta.ms`. The store keeps numbers and
  short words only (`normalize_voice`).
- Memory paths: the timeline; Honcho (`remember_turn`, session `secretary_lucinavoice`, label
  `[LucinaVoice speaker]`, the owner's peer); and each side's latest lines in the other's context
  (`voice_thread.rs`): an app turn gets the speaker session's recent turns, a spoken turn gets the app's latest
  timeline lines (12 h, 6 lines, 300 characters each). The platform rule (`prompts::VOICE_SURFACE_RULE`) tells the
  model the speaker is an owner device.

## Voice Learn

File: `lucina_vocab::default_path()` (`LUCINA_VOICE_LEARN_FILE`), the file the memo STT paths read.

| Route | |
|---|---|
| `GET /api/secretary/voice-learn` | `{entries, max}` |
| `POST /api/secretary/voice-learn` | `{word, soundsLike?, meaning?, lang?, source?}` -> 201 `{entry, memory}`; 400 on a rule (`validate_entry`, the 500 cap), 409 on a duplicate word |
| `GET\|PATCH\|DELETE /api/secretary/voice-learn/{id}` | PATCH merges, `null` clears `meaning`/`lang`; 404 for an unknown id |
| `POST /api/secretary/voice-learn/suggest` | `{transcript, corrected}` -> `{candidates:[{word, soundsLike, source:"correction", existingId?}]}`; nothing is stored |

Same bearer as every `/api/secretary` route. Writes read-modify-write the file under a lock (atomic save by
`lucina_vocab`).

- Meaning as context: every turn whose text holds an entry's word (whole word, case and accents ignored) gets
  `Vocabulary: <word> = <meaning>` in its memory context (`context::prepare_keyed`), at most five lines.
- Meaning as memory: an entry with a meaning is a Honcho conclusion (observer: Lucina's peer, observed: the owner)
  created on create/edit and deleted on delete/edit, mapped by `<vocab file>.memory.json` (entry id -> conclusion
  id + text), so the same text is never written twice. The response says `memory`: `written`, `removed`,
  `unchanged`, `skipped` (memory off or read-only) or `failed` (the call still succeeds).
- `vocab` layer: see JEV-ROUTING.md.

## Voice notes

`share/extract.rs` `audio()`: the Whisper branch is corrected inside `lucina_memo::transcribe`; the `on_device`
branch (the phone's own transcript) goes through `voice_learn::correct_transcript`: `Vocab::correct`, then, if
spans are ambiguous, the same `vocab` layer as a spoken turn (300 ms, fail open; a voice note has no turn trace,
so there is no shared call count: it is one call). The corrected text is what the memory record and the turn read;
the owner's timeline item keeps the phone's transcript as the phone made it. Meanings reach the note's turn through
the usual `Vocabulary:` context lines (a voice-only note's message is the transcript; a mixed share holds it in the
shared content).
