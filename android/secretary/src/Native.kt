package tech.getsecretary.secretary

import android.content.Context

/**
 * JNI surface of liblucina_calls.so (crates/lucina-android-calls, see docs/lucina-android.md).
 * Every function returns JSON. All but [phoneCall] and [shareUpload] return the envelope `{"ok", "data" | "error"}`,
 * which [unwrap] opens. [journal], [ui] and the caller state calls are cheap; [voiceTurn], [transcribe],
 * [shareUpload] and [screenCall] block on the network and must not run on the main thread.
 */
object Native {
    init {
        System.loadLibrary("lucina_calls")
    }

    /** Lucina's phone function: `{"status": "needs_permission" | "ready" | "submitted" | "error", ...}`. */
    @JvmStatic external fun phoneCall(context: Context, number: String, dryRun: Boolean): String

    /** The private call journal: `begin`, `append`, `end`, `history`, `view`. */
    @JvmStatic external fun journal(filesDir: String, op: String, json: String): String

    /** One voice turn with Lucina, `{"id", "text"}` -> `{"reply", "sensitive", "speak"}`. */
    @JvmStatic external fun voiceTurn(filesDir: String, cookie: String, json: String): String

    /** Transcribe a recording into the journal, `{"consent", "mimeType"?, "id"?}` -> `{"id", "saved", "partial"}`. */
    @JvmStatic external fun transcribe(filesDir: String, cookie: String, json: String, audio: ByteArray): String

    /** Streaming upload of a staged share; returns {ok, status, retry, shareId, error?}. */
    @JvmStatic external fun shareUpload(filesDir: String, cookie: String, manifestPath: String): String

    /** Screen decisions: `callView`, `dtmf`, `route`. */
    @JvmStatic external fun ui(op: String, json: String): String

    /** The phone app's own screens (`label`, `callLog`, `missed`, `voicemail`), an envelope. Cheap; never the emergency path. */
    @JvmStatic external fun dialer(filesDir: String, op: String, json: String): String

    /** Sends the queued rejection and allow reports to Lucina (an envelope `{waiting}`). Blocks on the network. */
    @JvmStatic external fun callerFlush(filesDir: String, cookie: String): String

    /** Caller screening state, an envelope: `{settings, last, lastLine}`. Cheap. */
    @JvmStatic external fun callerState(filesDir: String): String

    /** Changes the owner's switches `{label?, reject?: {enabled?, spamMin?, pickUpMax?, orderSpamMin?}}`; returns the state. Cheap. */
    @JvmStatic external fun callerSettings(filesDir: String, json: String): String

    /** The instant, offline verdict for a ringing number: an envelope around `{notify, action, title, text, sub, ...}`. */
    @JvmStatic external fun localVerdict(filesDir: String, number: String, json: String, lang: String): String

    /** An Obsidian vault as plain files: one request of `lucina-vault`'s protocol on the folder [root] (docs/lucina-vault.md). Reads the disk. */
    @JvmStatic external fun vault(root: String, json: String): String

    /** Jev's verdict from the server (about 1.2 s at most), else the rules; same shape. Blocks on the network. */
    @JvmStatic external fun screenCall(filesDir: String, cookie: String, number: String, json: String, lang: String): String
}
