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
import android.os.SystemClock
import android.provider.ContactsContract
import android.net.Uri
import android.telecom.Call
import android.telecom.CallScreeningService
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
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
        // Telecom may ask this service twice for one call (as the call-screening app and as the phone app):
        // the second ask waits for the first one's verdict and gives the same answer, so a number is looked up once.
        val key = number.ifEmpty { "private" }
        val mine = Flight()
        flights.entries.removeIf { SystemClock.elapsedRealtime() - it.value.startedAt > SAME_CALL_MS }
        val prior = flights.putIfAbsent(key, mine)
        if (prior != null && SystemClock.elapsedRealtime() - prior.startedAt < SAME_CALL_MS) {
            if (waits) {
                Handler(Looper.getMainLooper()).postDelayed({ answer("allow") }, WAIT_MS)
                Thread {
                    prior.done.await(WAIT_MS, TimeUnit.MILLISECONDS)
                    answer(prior.action)
                }.start()
            } else {
                answer("allow")
            }
            return
        }
        if (prior != null) flights[key] = mine
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
                // As the phone app, the call screen shows the label itself (and its heads-up has the buttons);
                // a second heads-up would only repeat it.
                val own = !Phone.isDefaultDialer(app)
                if (own) post(app, number, unwrap(Native.localVerdict(dir, number, json, lang)))
                val verdict = unwrap(Native.screenCall(dir, cookie, number, json, lang))
                if (own) post(app, number, verdict)
                action = verdict.optString("action", "allow")
            } catch (e: Throwable) {
                // Screening never gets in the way of the call.
            } finally {
                mine.action = action
                mine.done.countDown()
                answer(action)
                // The verdict is now in the cache: refresh the call screen and its notification.
                try {
                    LucinaCallService.live?.refresh()
                } catch (e: Throwable) {
                }
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

    /** One screening of one call, shared by a second ask for the same number. */
    private class Flight {
        val startedAt = SystemClock.elapsedRealtime()
        val done = CountDownLatch(1)

        @Volatile
        var action = "allow"
    }

    companion object {
        /** The longest the answer waits for Jev, when silence or reject is on. */
        const val WAIT_MS = 1500L

        /** Two asks for one number this close together are one call. */
        private const val SAME_CALL_MS = 15_000L
        private val flights = ConcurrentHashMap<String, Flight>()
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
