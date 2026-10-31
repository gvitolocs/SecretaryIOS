package tech.getsecretary.secretary

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.widget.Toast
import org.json.JSONObject

/**
 * The quiet note after a rejection, and what its buttons do. "Allow & call back" is the main action: on his tap
 * it allows the number (never rejected again, reported to Lucina) and places the call through Telecom. "Allow"
 * only allows. Nothing here runs without his tap.
 */
object RejectNotice {
    private const val CHANNEL = "rejected_calls"
    const val ALLOW = "tech.getsecretary.secretary.action.REJECT_ALLOW"
    const val ALLOW_CALL = "tech.getsecretary.secretary.action.REJECT_ALLOW_CALL"

    fun id(rejection: String): Int = 4000 + (rejection.hashCode() and 0xfff)

    /** Low importance: no sound, no vibration, no heads-up. */
    fun post(c: Context, rejection: String, number: String, line: String) {
        val nm = c.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, "Rejected calls", NotificationManager.IMPORTANCE_LOW).apply {
                description = "Calls the auto-reject rule turned away, with the way to allow them."
                setSound(null, null)
                enableVibration(false)
            },
        )
        val open = PendingIntent.getActivity(
            c, 7, Intent(c, CallerScreenActivity::class.java), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val b = Notification.Builder(c, CHANNEL)
            .setSmallIcon(android.R.drawable.sym_call_missed)
            .setContentTitle("Call rejected")
            .setContentText(line)
            .setStyle(Notification.BigTextStyle().bigText(line))
            .setCategory(Notification.CATEGORY_STATUS)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
            .setOnlyAlertOnce(true)
            .setAutoCancel(true)
            .setContentIntent(open)
            .addAction(action(c, ALLOW_CALL, "Allow & call back", rejection, number))
            .addAction(action(c, ALLOW, "Allow", rejection, number))
        try {
            nm.notify(id(rejection), b.build())
        } catch (e: SecurityException) {
            // Notifications are not allowed; the rejection is still logged and shown in the Callers screen.
        }
    }

    fun action(c: Context, kind: String, label: String, rejection: String, number: String): Notification.Action {
        val intent = Intent(c, RejectActionReceiver::class.java).setAction(kind).setPackage(c.packageName)
            .putExtra("id", rejection).putExtra("number", number)
        val code = id(rejection) * 2 + if (kind == ALLOW_CALL) 1 else 0
        return Notification.Action.Builder(null, label, PendingIntent.getBroadcast(c, code, intent, PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)).build()
    }

    /** Allows [number] in Rust (list and report), flushes the report, and optionally calls back. Blocking: call off the main thread. */
    fun allow(c: Context, number: String, callBack: Boolean, cookie: String): String? {
        try {
            unwrap(Native.dialer(c.filesDir.absolutePath, "allow", JSONObject().put("number", number).toString()))
        } catch (t: Throwable) {
            return "Could not allow this number."
        }
        safely(Unit) { Native.callerFlush(c.filesDir.absolutePath, cookie) }
        if (callBack) {
            val out = Phone.dial(c, number)
            if (out !is DialPlan.Outcome.Placed) return "Allowed, but the call could not be placed."
        }
        return null
    }
}

/** The buttons of the rejection note. Not exported: only this app's own PendingIntents reach it. */
class RejectActionReceiver : BroadcastReceiver() {
    override fun onReceive(c: Context, i: Intent) {
        val number = i.getStringExtra("number") ?: return
        val id = i.getStringExtra("id") ?: ""
        val callBack = i.action == RejectNotice.ALLOW_CALL
        if (!callBack && i.action != RejectNotice.ALLOW) return
        val app = c.applicationContext
        val cookie = safely("") { Calls.cookie() } // CookieManager is read on the main thread
        val pending = goAsync()
        Thread {
            try {
                val problem = RejectNotice.allow(app, number, callBack, cookie)
                safely(Unit) { app.getSystemService(NotificationManager::class.java).cancel(RejectNotice.id(id)) }
                if (problem != null) {
                    android.os.Handler(android.os.Looper.getMainLooper()).post { Toast.makeText(app, problem, Toast.LENGTH_LONG).show() }
                }
            } finally {
                pending.finish()
            }
        }.start()
    }
}
