package tech.getsecretary.secretary

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper
import android.provider.ContactsContract
import android.net.Uri
import android.telecom.Call
import android.telecom.CallScreeningService
import java.util.Locale
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONObject

/**
 * Screens incoming calls with Jev through Rust (`Native.localVerdict` / `Native.screenCall`).
 *
 * The call is allowed at once. It is silenced or rejected only when the owner turned that switch on himself and
 * Rust says the verdict is Jev's and reaches the threshold; then the answer waits for the verdict, but never
 * longer than [WAIT_MS], far inside the system's 5 s. The call log and the missed-call notification are never skipped.
 * A heads-up notification shows the instant rules verdict and is replaced by the server's. Nothing is logged.
 */
class CallerScreenService : CallScreeningService() {
    override fun onScreenCall(details: Call.Details) {
        if (details.callDirection != Call.Details.DIRECTION_INCOMING) {
            respondToCall(details, response("allow"))
            return
        }
        val app = applicationContext
        val dir = filesDir.absolutePath
        val number = try {
            details.handle?.schemeSpecificPart ?: ""
        } catch (e: Exception) {
            ""
        }
        val lang = Locale.getDefault().language
        val waits = try {
            unwrap(Native.callerState(dir)).getJSONObject("settings").let { it.optBoolean("silence") || it.optBoolean("reject") }
        } catch (e: Exception) {
            false
        }
        val answered = AtomicBoolean(false)
        val answer = { action: String ->
            if (answered.compareAndSet(false, true)) respondToCall(details, response(action))
        }
        if (waits) {
            // Whatever happens below, the call is allowed before the system's limit.
            Handler(Looper.getMainLooper()).postDelayed({ answer("allow") }, WAIT_MS)
        } else {
            answer("allow")
        }
        // The WebView session authenticates the request; CookieManager is read on the main thread.
        val cookie = try {
            Calls.cookie()
        } catch (e: Exception) {
            ""
        }
        Thread {
            var action = "allow"
            try {
                val input = JSONObject()
                contactName(app, number)?.let { input.put("contact", it) }
                val json = input.toString()
                val first = unwrap(Native.localVerdict(dir, number, json, lang))
                post(app, number, first)
                val verdict = unwrap(Native.screenCall(dir, cookie, number, json, lang))
                post(app, number, verdict)
                action = verdict.optString("action", "allow")
            } catch (e: Exception) {
                // Screening never gets in the way of the call.
            } finally {
                answer(action)
            }
        }.start()
    }

    private fun response(action: String): CallResponse {
        val b = CallResponse.Builder()
        when (action) {
            "silence" -> b.setSilenceCall(true)
            "reject" -> b.setDisallowCall(true).setRejectCall(true).setSkipCallLog(false).setSkipNotification(false)
        }
        return b.build()
    }

    companion object {
        /** The longest the answer waits for Jev, when silence or reject is on. */
        const val WAIT_MS = 1500L
        private const val CHANNEL = "caller_screening"
        private const val TAG = "caller"
        private const val TIMEOUT_MS = 120_000L

        /** The contact's name, if the owner allowed contacts and the number is in them. */
        private fun contactName(c: Context, number: String): String? {
            if (number.isEmpty() || c.checkSelfPermission(Manifest.permission.READ_CONTACTS) != PackageManager.PERMISSION_GRANTED) return null
            return try {
                val uri = Uri.withAppendedPath(ContactsContract.PhoneLookup.CONTENT_FILTER_URI, Uri.encode(number))
                c.contentResolver.query(uri, arrayOf(ContactsContract.PhoneLookup.DISPLAY_NAME), null, null, null)?.use {
                    if (it.moveToFirst()) it.getString(0) else null
                }
            } catch (e: Exception) {
                null
            }
        }

        /** Shows or replaces the heads-up notification for [number] (one per number). */
        private fun post(c: Context, number: String, s: JSONObject) {
            if (!s.optBoolean("notify")) return
            val nm = c.getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(CHANNEL, "Caller screening", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "Who is calling, as Jev sees it, while the phone rings."
                    setSound(null, null) // the phone is already ringing
                    enableVibration(false)
                },
            )
            val text = s.optString("text")
            val open = PendingIntent.getActivity(
                c, 3, Intent(c, CallerScreenActivity::class.java),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            val builder = Notification.Builder(c, CHANNEL)
                .setSmallIcon(android.R.drawable.sym_action_call)
                .setContentTitle(s.optString("title"))
                .setContentText(text.substringBefore('\n'))
                .setStyle(Notification.BigTextStyle().bigText(text))
                .setSubText(s.optString("sub"))
                .setCategory(Notification.CATEGORY_STATUS)
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setOnlyAlertOnce(true)
                .setAutoCancel(true)
                .setTimeoutAfter(TIMEOUT_MS)
                .setContentIntent(open)
            if (s.optDouble("spam") >= 0.7) builder.setColor(0xFFFF453A.toInt())
            try {
                nm.notify(TAG, (number.hashCode() and 0x3fff) + 3000, builder.build())
            } catch (e: SecurityException) {
                // Notifications are not allowed yet; the call is unaffected.
            }
        }
    }
}
