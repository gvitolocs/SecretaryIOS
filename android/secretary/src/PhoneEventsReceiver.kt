package tech.getsecretary.secretary

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.telecom.TelecomManager
import android.telephony.TelephonyManager
import java.util.Locale
import org.json.JSONObject

/**
 * What the default phone app must do for the system: show the missed-call notification
 * (`TelecomManager.ACTION_SHOW_MISSED_CALLS_NOTIFICATION`) and the voicemail notification
 * (`TelephonyManager.ACTION_SHOW_VOICEMAIL_NOTIFICATION`). Only the system sends them; the manifest requires
 * the sender to hold READ_PHONE_STATE. Rust words the notification; if it fails, a plain one is shown.
 */
class PhoneEventsReceiver : BroadcastReceiver() {
    override fun onReceive(c: Context, i: Intent) {
        safely(Unit) {
            when (i.action) {
                TelecomManager.ACTION_SHOW_MISSED_CALLS_NOTIFICATION -> missed(
                    c,
                    i.getIntExtra(TelecomManager.EXTRA_NOTIFICATION_COUNT, 0),
                    i.getStringExtra(TelecomManager.EXTRA_NOTIFICATION_PHONE_NUMBER) ?: "",
                )
                TelephonyManager.ACTION_SHOW_VOICEMAIL_NOTIFICATION -> voicemail(
                    c,
                    i.getIntExtra(TelephonyManager.EXTRA_NOTIFICATION_COUNT, -1),
                    @Suppress("DEPRECATION") i.getParcelableExtra<PendingIntent>(TelephonyManager.EXTRA_CALL_VOICEMAIL_INTENT),
                )
            }
        }
    }

    private fun channel(c: Context, id: String, name: String): NotificationManager {
        val nm = c.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(id, name, NotificationManager.IMPORTANCE_DEFAULT))
        return nm
    }

    private fun dialer(c: Context, op: String, req: JSONObject): JSONObject? = safely(null) {
        unwrap(Native.dialer(c.filesDir.absolutePath, op, req.put("lang", Locale.getDefault().language).toString()))
    }

    private fun missed(c: Context, count: Int, number: String) {
        val nm = channel(c, "missed_calls", "Missed calls")
        if (count <= 0) {
            nm.cancel(MISSED_NOTIFICATION)
            return
        }
        val name = LucinaCallService.contactName(c, number)
        val text = dialer(c, "missed", JSONObject().put("count", count).put("number", number).put("name", name ?: JSONObject.NULL))
        val open = PendingIntent.getActivity(c, 5, Intent(c, CallLogActivity::class.java), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        nm.notify(
            MISSED_NOTIFICATION,
            Notification.Builder(c, "missed_calls")
                .setSmallIcon(android.R.drawable.sym_call_missed)
                .setContentTitle(text?.optString("title") ?: if (count == 1) "Missed call" else "$count missed calls")
                .setContentText(text?.optString("text") ?: number.ifEmpty { "Private number" })
                .setCategory(Notification.CATEGORY_MISSED_CALL)
                .setAutoCancel(true)
                .setContentIntent(open)
                .build(),
        )
    }

    private fun voicemail(c: Context, count: Int, call: PendingIntent?) {
        val nm = channel(c, "voicemail", "Voicemail")
        val text = dialer(c, "voicemail", JSONObject().put("count", count))
        if (text?.optBoolean("clear") == true || count == 0) {
            nm.cancel(VOICEMAIL_NOTIFICATION)
            return
        }
        val open = call ?: PendingIntent.getActivity(c, 6, Intent(c, CallsActivity::class.java), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        nm.notify(
            VOICEMAIL_NOTIFICATION,
            Notification.Builder(c, "voicemail")
                .setSmallIcon(android.R.drawable.stat_notify_voicemail)
                .setContentTitle(text?.optString("title") ?: "New voicemail")
                .setContentText(text?.optString("text") ?: "Tap to call your voicemail.")
                .setAutoCancel(true)
                .setContentIntent(open)
                .build(),
        )
    }

    companion object {
        const val MISSED_NOTIFICATION = 2002
        const val VOICEMAIL_NOTIFICATION = 2003
    }
}
