package tech.getsecretary.lucinaotp

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.net.Uri
import android.os.Binder
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.os.Process
import android.util.Log
import java.io.ByteArrayOutputStream
import java.io.FileNotFoundException
import java.io.IOException
import java.nio.charset.StandardCharsets
import org.json.JSONObject

/**
 * adb-only pairing and diagnostics, as in 0.4.0:
 *   adb shell content call --uri content://tech.getsecretary.lucinaotp.pair --method status|selftest|cleanup
 *   adb shell content write --uri content://tech.getsecretary.lucinaotp.pair/configure < config.json
 */
class PairingProvider : ContentProvider() {
    override fun onCreate(): Boolean = true

    private fun authorize() {
        val uid = Binder.getCallingUid()
        if (uid != 2000 && uid != 0 && uid != Process.myUid()) throw SecurityException("Authenticated ADB pairing only")
    }

    override fun call(method: String, arg: String?, extras: Bundle?): Bundle {
        authorize()
        val context = context ?: throw IllegalStateException("no context")
        val out = Bundle()
        when (method) {
            "cleanup" -> {
                MainActivity.cleanupLegacyNotification(context)
                out.putBoolean("cleaned", true)
            }
            "status" -> {
                Core.ensure(context)
                val json = Native.status()
                val s = JSONObject(json)
                out.putString("json", json)
                out.putBoolean("paired", s.optBoolean("paired"))
                out.putString("version", s.optString("version"))
                out.putBoolean("rustSelfTest", s.optBoolean("selfTest"))
            }
            "selftest" -> {
                Core.ensure(context)
                val json = Native.selfTest(extras?.getBoolean("network", true) ?: true)
                val r = JSONObject(json)
                val result = r.optString("result")
                Log.i(TAG, "selftest result=$result ms=${r.optLong("ms")}")
                out.putString("json", json)
                out.putString("result", result)
                out.putLong("ms", r.optLong("ms"))
                out.putBoolean("parse", r.optBoolean("parse"))
                out.putBoolean("submitted", result == "sent" || result == "accepted")
            }
            else -> throw IllegalArgumentException("Unknown pairing method")
        }
        return out
    }

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        authorize()
        if (mode != "w" || uri.path != "/configure") throw FileNotFoundException()
        val context = context ?: throw FileNotFoundException()
        try {
            val pipe = ParcelFileDescriptor.createPipe()
            Thread({
                try {
                    ParcelFileDescriptor.AutoCloseInputStream(pipe[0]).use { input ->
                        val buf = ByteArrayOutputStream()
                        val chunk = ByteArray(512)
                        while (true) {
                            val n = input.read(chunk)
                            if (n < 0) break
                            if (buf.size() + n > 2048) return@Thread
                            buf.write(chunk, 0, n)
                        }
                        val bytes = buf.toByteArray()
                        val json = String(bytes, StandardCharsets.UTF_8)
                        bytes.fill(0)
                        Core.ensure(context)
                        val paired = JSONObject(Native.configure(json)).optBoolean("paired")
                        Log.i(TAG, "configure result=${if (paired) "paired" else "unpaired"}")
                    }
                } catch (t: Throwable) {
                    Log.w(TAG, "configure result=error")
                }
            }, "lucina-pairing").start()
            return pipe[1]
        } catch (e: IOException) {
            throw FileNotFoundException("Pairing unavailable")
        }
    }

    private fun denied() = SecurityException("No SMS data interface")
    override fun query(u: Uri, p: Array<String>?, s: String?, a: Array<String>?, o: String?): Cursor = throw denied()
    override fun getType(u: Uri): String = throw denied()
    override fun insert(u: Uri, v: ContentValues?): Uri = throw denied()
    override fun delete(u: Uri, s: String?, a: Array<String>?): Int = throw denied()
    override fun update(u: Uri, v: ContentValues?, s: String?, a: Array<String>?): Int = throw denied()
}
