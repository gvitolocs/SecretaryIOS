package tech.getsecretary.secretary

import android.content.Context

/**
 * JNI surface of liblucina_calls.so (crates/lucina-android-calls, see docs/lucina-android.md).
 * Every function returns JSON. All but [phoneCall] and [shareUpload] return the envelope `{"ok", "data" | "error"}`,
 * which [unwrap] opens. [journal] and [ui] are cheap; [voiceTurn], [transcribe] and [shareUpload] block on the
 * network and must not run on the main thread.
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
}
