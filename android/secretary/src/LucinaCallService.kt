package tech.getsecretary.secretary

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.telecom.Call
import android.telecom.InCallService
import java.util.concurrent.CopyOnWriteArrayList
import org.json.JSONArray
import org.json.JSONObject

/**
 * The dialer-role InCallService: tracks Telecom's calls, journals each SIM call, and shows the
 * ongoing-call notification Rust describes (full-screen while ringing).
 */
class LucinaCallService : InCallService() {
    /** Journal id per call; only touched on [Calls.io]. */
    private val ids = HashMap<Call, String>()

    private val callback = object : Call.Callback() {
        override fun onStateChanged(call: Call, state: Int) {
            changed()
        }
    }

    override fun onCreate() {
        super.onCreate()
        live = this
        getSystemService(NotificationManager::class.java)
            .createNotificationChannel(NotificationChannel(CHANNEL, "Phone calls", NotificationManager.IMPORTANCE_HIGH))
    }

    override fun onCallAdded(call: Call) {
        super.onCallAdded(call)
        calls.add(call)
        val peer = try {
            view(listOf(call)).getJSONArray("calls").getJSONObject(0).getString("peer")
        } catch (e: Exception) {
            "Phone call"
        }
        val app = applicationContext
        Calls.io.execute {
            try {
                ids[call] = Calls.begin(app, "sim", peer)
            } catch (e: Exception) {
            }
        }
        call.registerCallback(callback)
        changed()
    }

    override fun onCallRemoved(call: Call) {
        call.unregisterCallback(callback)
        val app = applicationContext
        Calls.io.execute {
            val id = ids.remove(call)
            if (id != null) {
                try {
                    Calls.journal(app, "end", JSONObject().put("id", id).put("status", "disconnected"))
                } catch (e: Exception) {
                }
            }
        }
        calls.remove(call)
        super.onCallRemoved(call)
        changed()
    }

    override fun onDestroy() {
        live = null
        super.onDestroy()
    }

    /** Refresh the notification and tell an open [InCallActivity]. */
    private fun changed() {
        notifyCall()
        sendBroadcast(Intent(CHANGED).setPackage(packageName))
    }

    private fun notifyCall() {
        val nm = getSystemService(NotificationManager::class.java)
        val snapshot = ArrayList(calls)
        val n = try {
            view(snapshot).optJSONObject("notification")
        } catch (e: Exception) {
            null
        }
        if (n == null) {
            if (snapshot.isEmpty()) nm.cancel(NOTIFICATION)
            return
        }
        val ui = Intent(this, InCallActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val pending = PendingIntent.getActivity(this, 1, ui, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val builder = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.sym_action_call)
            .setContentTitle(n.optString("title"))
            .setContentText(n.optString("text"))
            .setCategory(Notification.CATEGORY_CALL)
            .setOngoing(true)
            .setContentIntent(pending)
        if (n.optBoolean("fullScreen")) builder.setFullScreenIntent(pending, true)
        nm.notify(NOTIFICATION, builder.build())
    }

    companion object {
        const val CHANGED = "tech.getsecretary.secretary.CALL_CHANGED"
        private const val CHANNEL = "calls"
        private const val NOTIFICATION = 2001

        @Volatile
        var live: LucinaCallService? = null

        val calls: MutableList<Call> = CopyOnWriteArrayList()

        /** Rust's in-call screen and notification for [list] (`ui` op `callView`). */
        @Suppress("DEPRECATION") // Call.Details.getState() needs API 31; minSdk is 29.
        fun view(list: List<Call>): JSONObject {
            val arr = JSONArray()
            for (c in list) {
                val handle = try {
                    c.details.handle?.schemeSpecificPart
                } catch (e: Exception) {
                    null
                }
                arr.put(
                    JSONObject()
                        .put("state", c.state)
                        .put("handle", handle ?: JSONObject.NULL)
                        .put("canHold", c.details.can(Call.Details.CAPABILITY_HOLD)),
                )
            }
            return Calls.ui("callView", JSONObject().put("calls", arr))
        }
    }
}
