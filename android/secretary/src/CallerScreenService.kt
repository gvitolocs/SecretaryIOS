package tech.getsecretary.secretary

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.telecom.Call
import android.telecom.CallScreeningService
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONObject

/**
 * Screens incoming calls with Jev through Rust (`Native.localVerdict` / `Native.screenCall`) and applies the owner's
 * auto-reject rule, which is the shared core's `reject_decision` run in Rust.
 *
 * The phone-side guards (contact, called or answered before, emergency number) are read first. If one of them already
 * puts the call through, or one could not be checked, or the rule is off, the call is allowed at once and the verdict
 * only labels it. Otherwise the answer waits for the server's verdict, at most [WAIT_MS] (far inside the system's 5 s),
 * and rejects only if Rust says so in time; the fallback rules never reject. A rejected call stays in the call log and is
 * announced by a quiet note with the Allow buttons ([RejectNotice]). Nothing is logged.
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
        val checks = CallFacts.collect(app, number)
        val unchecked = ScreenPlan.unchecked(checks)
        val input = CallFacts.json(checks)
        input.put("number", number)
        val inputJson = input.toString()
        val waits = !unchecked && safely(false) { unwrap(Native.dialer(dir, "plan", inputJson)).optBoolean("wait") }
        val started = SystemClock.elapsedRealtime()
        val answered = AtomicBoolean(false)
        /** Sends the response once; true only if this call is the one that went out. */
        val answer = { action: String -> answered.compareAndSet(false, true).also { if (it) respondToCall(details, response(action)) } }
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
            var sent = "allow"
            try {
                // As the phone app, the call screen shows the label itself (and its heads-up has the buttons);
                // a second heads-up would only repeat it. While waiting to reject, no label is shown before the verdict.
                val own = !Phone.isDefaultDialer(app)
                if (own && !waits) post(app, number, unwrap(Native.localVerdict(dir, number, inputJson, lang)))
                val verdict = unwrap(Native.screenCall(dir, cookie, number, inputJson, lang))
                val inTime = SystemClock.elapsedRealtime() - started <= WAIT_MS
                val action = ScreenPlan.action(verdict.optString("action"), waits, unchecked, inTime)
                if (answer(action) && action == "reject") {
                    sent = "reject"
                    afterRejection(app, cookie, dir, verdict)
                } else if (own) {
                    post(app, number, verdict)
                }
            } catch (e: Throwable) {
                // Screening never gets in the way of the call.
            } finally {
                mine.action = sent
                mine.done.countDown()
                answer("allow")
                // The verdict is now in the cache: refresh the call screen and its notification.
                try {
                    LucinaCallService.live?.refresh()
                } catch (e: Throwable) {
                }
            }
        }.start()
    }

    private fun response(action: String): CallResponse {
        val r = ScreenPlan.response(action)
        return CallResponse.Builder()
            .setDisallowCall(r.disallow)
            .setRejectCall(r.reject)
            .setSkipCallLog(r.skipLog)
            .setSkipNotification(r.skipNotification)
            .build()
    }

    /** The call really was rejected: log it, tell Lucina (queued), and leave the quiet note with the Allow buttons. */
    private fun afterRejection(app: Context, cookie: String, dir: String, verdict: JSONObject) {
        val r = verdict.optJSONObject("reject") ?: return
        val logged = safely<JSONObject?>(null) { unwrap(Native.dialer(dir, "rejected", r.toString())) }
        RejectNotice.post(app, logged?.optString("id") ?: System.currentTimeMillis().toString(), r.optString("number"), r.optString("line"))
        safely(Unit) { Native.callerFlush(dir, cookie) }
    }

    /** One screening of one call, shared by a second ask for the same number. */
    private class Flight {
        val startedAt = SystemClock.elapsedRealtime()
        val done = CountDownLatch(1)

        @Volatile
        var action = "allow"
    }

    companion object {
        /** The longest the answer waits for Jev, when the call could be rejected. */
        const val WAIT_MS = 1500L

        /** Two asks for one number this close together are one call. */
        private const val SAME_CALL_MS = 15_000L
        private val flights = ConcurrentHashMap<String, Flight>()
        private const val CHANNEL = "caller_screening"
        private const val TAG = "caller"
        private const val TIMEOUT_MS = 120_000L

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
