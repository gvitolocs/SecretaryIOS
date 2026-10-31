package tech.getsecretary.secretary

import android.content.Context
import android.webkit.CookieManager
import java.util.TimeZone
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import org.json.JSONArray
import org.json.JSONObject

/** A Rust error message, shown to the owner as it is. */
class NativeException(message: String) : Exception(message)

/** Opens a Rust `{"ok", "data" | "error"}` envelope. */
fun unwrap(envelope: String): JSONObject {
    val r = JSONObject(envelope)
    if (!r.optBoolean("ok")) throw NativeException(r.optString("error", "Call operation unavailable."))
    return r.optJSONObject("data") ?: JSONObject()
}

/** Journal and screen helpers over [Native]. */
object Calls {
    const val SITE = "https://getsecretary.tech"

    /** Journal reads and writes leave the main thread but keep their order. */
    val io: ExecutorService = Executors.newSingleThreadExecutor()

    fun journal(c: Context, op: String, args: JSONObject): JSONObject =
        unwrap(Native.journal(c.filesDir.absolutePath, op, args.toString()))

    fun begin(c: Context, kind: String, peer: String): String =
        journal(c, "begin", JSONObject().put("kind", kind).put("peer", peer)).getString("id")

    /** Closes a journal record on [io]; failures are ignored. */
    fun endLater(c: Context, id: String, status: String) {
        val app = c.applicationContext
        io.execute {
            try {
                journal(app, "end", JSONObject().put("id", id).put("status", status))
            } catch (e: Exception) {
            }
        }
    }

    /** The transcript list, newest first, formatted in the phone's time zone. */
    fun view(c: Context): JSONArray {
        val offsetMin = TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 60000
        return journal(c, "view", JSONObject().put("tzOffsetMin", offsetMin)).getJSONArray("records")
    }

    fun ui(op: String, args: JSONObject): JSONObject = unwrap(Native.ui(op, args.toString()))

    /** The WebView session, which authenticates Rust's HTTPS requests. */
    fun cookie(): String = CookieManager.getInstance().getCookie(SITE) ?: ""
}
