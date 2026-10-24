package tech.getsecretary.lucinaotp

import android.content.Context
import android.util.Log
import org.json.JSONObject

internal const val TAG = "LucinaOtp"

/** Lazily opens the Rust core once per process. Never call on the main thread. */
internal object Core {
    @Volatile private var ready = false
    private val lock = Any()

    fun ensure(context: Context) {
        if (ready) return
        synchronized(lock) {
            if (ready) return
            val app = context.applicationContext
            val prefs = app.getSharedPreferences("bridge", Context.MODE_PRIVATE)
            val legacy = prefs.getString("internetToken", null)?.takeIf { it.isNotEmpty() }
            val key = KeyVault.dataKey(app)
            var status = try {
                JSONObject(Native.init(app.filesDir.absolutePath, key, legacy))
            } finally {
                key.fill(0)
            }
            // 0.4.0 kept the forwarding switch in SharedPreferences; hand it to the core once.
            if (prefs.contains("enabled")) {
                val enabled = prefs.getBoolean("enabled", false)
                status = JSONObject(Native.configure(JSONObject().put("enabled", enabled).toString()))
                prefs.edit().remove("enabled").commit()
            }
            if (legacy != null && status.optBoolean("paired")) {
                prefs.edit().remove("internetToken").commit()
            }
            ready = true
        }
    }

    /** Logs only the result and the elapsed time, never a body, code or token. */
    fun log(source: String, json: String): String {
        val r = JSONObject(json)
        val result = r.optString("result")
        Log.i(TAG, "$source result=$result ms=${r.optLong("ms")}")
        return result
    }
}
