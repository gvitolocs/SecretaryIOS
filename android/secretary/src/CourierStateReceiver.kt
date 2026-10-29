package tech.getsecretary.secretary

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import org.json.JSONObject

/**
 * Lucina Maps tells this phone about the Wolt shift: `tech.getsecretary.secretary.action.COURIER_STATE` with
 * `onShift: Boolean` and `orderAtMs: Long` (the last Wolt link, 0 = none). The manifest protects the receiver with
 * the `COURIER_STATE` permission, which Lucina Maps requests. Rust validates the values and keeps them; they only
 * colour the screening verdict, and a wrong one can never silence or reject a call.
 */
class CourierStateReceiver : BroadcastReceiver() {
    override fun onReceive(c: Context, i: Intent) {
        if (i.action != ACTION) return
        try {
            val state = JSONObject()
                .put("onShift", i.getBooleanExtra("onShift", false))
                .put("orderAtMs", maxOf(0L, i.getLongExtra("orderAtMs", 0L)))
            unwrap(Native.setCourier(c.filesDir.absolutePath, state.toString()))
        } catch (e: Exception) {
            // A report Rust refuses is ignored.
        }
    }

    companion object {
        const val ACTION = "tech.getsecretary.secretary.action.COURIER_STATE"
    }
}
