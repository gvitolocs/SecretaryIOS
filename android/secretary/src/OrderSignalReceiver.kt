package tech.getsecretary.secretary

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import org.json.JSONObject

/**
 * Lucina Maps tells this phone when a navigation link arrives (`orderAtMs`, ms since the epoch). It is used for one
 * thing only: while that link is under 45 minutes old, auto-reject needs spam of 0.97 or more instead of 0.60. It is
 * kept in a private file, never sent to the server or to Jev. The manifest requires the sender to hold the
 * ORDER_SIGNAL permission, which Lucina Maps requests; Rust validates the value.
 */
class OrderSignalReceiver : BroadcastReceiver() {
    override fun onReceive(c: Context, i: Intent) {
        if (i.action != ACTION) return
        safely(Unit) {
            val at = maxOf(0L, i.getLongExtra("orderAtMs", 0L))
            unwrap(Native.dialer(c.filesDir.absolutePath, "order", JSONObject().put("orderAtMs", at).toString()))
        }
    }

    companion object {
        const val ACTION = "tech.getsecretary.secretary.action.ORDER_SIGNAL"
    }
}
