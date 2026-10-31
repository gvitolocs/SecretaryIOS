package tech.getsecretary.secretary

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.net.Uri
import android.os.Binder
import android.os.Bundle
import android.os.Process

/**
 * Lucina's phone MCP over authenticated adb:
 * `content call --uri content://tech.getsecretary.secretary.calls --method call|check --arg <number>`.
 * Returns `{"result": <Native.phoneCall JSON>}`; the same Rust function as the Call button.
 */
class CallProvider : ContentProvider() {
    override fun onCreate(): Boolean = true

    override fun call(method: String, arg: String?, extras: Bundle?): Bundle {
        val uid = Binder.getCallingUid()
        if (uid != 2000 && uid != 0 && uid != Process.myUid()) throw SecurityException("Owner-authorized device bridge only")
        if (method != "call" && method != "check") throw IllegalArgumentException("Unknown phone function")
        val context = context ?: throw IllegalStateException("Provider not attached")
        val b = Bundle()
        b.putString("result", Native.phoneCall(context, arg ?: "", method == "check"))
        return b
    }

    override fun query(u: Uri, p: Array<String>?, s: String?, a: Array<String>?, o: String?): Cursor = throw SecurityException()

    override fun getType(u: Uri): String? = null

    override fun insert(u: Uri, v: ContentValues?): Uri = throw SecurityException()

    override fun delete(u: Uri, s: String?, a: Array<String>?): Int = throw SecurityException()

    override fun update(u: Uri, v: ContentValues?, s: String?, a: Array<String>?): Int = throw SecurityException()
}
