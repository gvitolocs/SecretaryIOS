package tech.getsecretary.secretary

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.PowerManager
import android.provider.ContactsContract
import android.telecom.Call
import android.telecom.CallAudioState
import android.telecom.InCallService
import java.util.Locale
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.CopyOnWriteArrayList
import org.json.JSONObject

/**
 * The phone app's InCallService. Telecom binds it for every call, including emergency calls, so everything
 * here is guarded: a failing Rust core, contacts lookup or notification never stops the call from showing up
 * ([CallPlan.fallback] is the screen without Rust).
 *
 * Incoming calls get a full-screen intent (screen off or locked) or a heads-up (screen in use, for example
 * Wolt in the foreground) with Answer and Decline buttons that act only on the owner's tap. The system plays
 * the ringtone (`IN_CALL_SERVICE_RINGING` is false), so the notification channels are silent.
 */
class LucinaCallService : InCallService() {
    /** Journal id per call; only touched on [Calls.io]. */
    private val ids = HashMap<Call, String>()
    private var proximity: PowerManager.WakeLock? = null

    private val callback = object : Call.Callback() {
        override fun onStateChanged(call: Call, state: Int) {
            changed()
        }

        override fun onDetailsChanged(call: Call, details: Call.Details) {
            changed()
        }
    }

    override fun onCreate() {
        super.onCreate()
        live = this
        safely(Unit) { channels(this) }
    }

    override fun onCallAdded(call: Call) {
        super.onCallAdded(call)
        Companion.calls.add(call) // not InCallService.getCalls(), which is read-only
        safely(Unit) { info[call] = identify(this, call) }
        val peer = safely("Phone call") { CallPlan.peer(call.details.handle?.schemeSpecificPart) }
        val app = applicationContext
        // An emergency call is not written to the journal.
        if (!safely(true) { Phone.isEmergency(app, DialPlan.normalize(handle(call) ?: "")) }) {
            Calls.io.execute {
                safely(Unit) { ids[call] = Calls.begin(app, "sim", peer) }
            }
        }
        call.registerCallback(callback)
        changed()
        // Outgoing calls open the screen at once (the dialer started them); incoming ones come with the notification.
        if (call.state == Call.STATE_DIALING || call.state == Call.STATE_CONNECTING) safely(Unit) { openScreen() }
    }

    override fun onCallRemoved(call: Call) {
        call.unregisterCallback(callback)
        val app = applicationContext
        Calls.io.execute {
            val id = ids.remove(call)
            if (id != null) {
                safely(Unit) { Calls.journal(app, "end", JSONObject().put("id", id).put("status", "disconnected")) }
            }
        }
        Companion.calls.remove(call)
        info.remove(call)
        super.onCallRemoved(call)
        changed()
    }

    @Suppress("DEPRECATION") // The endpoint API replaces it on API 34; this phone runs Android 13.
    override fun onCallAudioStateChanged(state: CallAudioState) {
        audio = CallViews.Audio(state.isMuted, state.route, state.supportedRouteMask)
        changed()
    }

    override fun onBringToForeground(showDialpad: Boolean) {
        safely(Unit) { openScreen() }
    }

    override fun onDestroy() {
        live = null
        safely(Unit) { setProximity(false) }
        super.onDestroy()
    }

    private fun openScreen() {
        startActivity(Intent(this, InCallActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP))
    }

    /** A new Jev verdict arrived (the screening service): redraw the notification and the call screen. */
    fun refresh() = changed()

    /** Refresh the notification, the proximity lock and an open [InCallActivity]. */
    private fun changed() {
        val v = view(this)
        safely(Unit) { notifyCall(v) }
        safely(Unit) { setProximity(v.controls.proximity) }
        safely(Unit) { sendBroadcast(Intent(CHANGED).setPackage(packageName)) }
    }

    private fun setProximity(on: Boolean) {
        val pm = getSystemService(PowerManager::class.java)
        if (on) {
            if (proximity == null && pm.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) {
                proximity = pm.newWakeLock(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK, "secretary:call").apply {
                    setReferenceCounted(false)
                    acquire(6 * 3_600_000L)
                }
            }
        } else {
            proximity?.let { if (it.isHeld) it.release() }
            proximity = null
        }
    }

    private fun notifyCall(v: CallPlan.View) {
        val nm = getSystemService(NotificationManager::class.java)
        val n = v.notice
        if (n == null) {
            nm.cancel(NOTIFICATION)
            return
        }
        val ringing = n.title == "Incoming call"
        val screen = PendingIntent.getActivity(
            this, 1,
            Intent(this, InCallActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val b = Notification.Builder(this, if (ringing) INCOMING else ONGOING)
            .setSmallIcon(android.R.drawable.sym_action_call)
            .setContentTitle(n.title)
            .setContentText(n.text)
            .setCategory(Notification.CATEGORY_CALL)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setContentIntent(screen)
        n.sub?.let { b.setSubText(it) }
        val call = Companion.calls.getOrNull(n.index)
        if (!ringing && call != null && call.state == Call.STATE_ACTIVE) {
            val since = call.details.connectTimeMillis
            if (since > 0) b.setWhen(since).setShowWhen(true).setUsesChronometer(true)
        }
        if (n.fullScreen) b.setFullScreenIntent(screen, true)
        for (a in n.actions) {
            when (a) {
                "answer" -> b.addAction(action(10, CallActionReceiver.ANSWER, "Answer"))
                "decline" -> b.addAction(action(11, CallActionReceiver.DECLINE, "Decline"))
                "hangup" -> b.addAction(action(12, CallActionReceiver.HANGUP, "Hang up"))
            }
        }
        nm.notify(NOTIFICATION, b.build())
    }

    private fun action(code: Int, action: String, label: String): Notification.Action {
        val pi = PendingIntent.getBroadcast(
            this, code, Intent(this, CallActionReceiver::class.java).setAction(action).setPackage(packageName),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Action.Builder(null, label, pi).build()
    }

    /** What is known about one call beyond Telecom's own details. */
    class Info(val incoming: Boolean, val number: String, var name: String?, var jev: String?, var spam: Double)

    companion object {
        const val CHANGED = "tech.getsecretary.secretary.CALL_CHANGED"
        private const val INCOMING = "calls_incoming"
        private const val ONGOING = "calls_ongoing"
        private const val NOTIFICATION = 2001

        @Volatile
        var live: LucinaCallService? = null

        @Volatile
        var audio = CallViews.Audio(false, CallAudioState.ROUTE_EARPIECE, 0)

        val calls: MutableList<Call> = CopyOnWriteArrayList()
        private val info = ConcurrentHashMap<Call, Info>()

        /** Silent channels: Telecom plays the ringtone, the notification only has to be seen. */
        fun channels(c: Context) {
            val nm = c.getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(
                NotificationChannel(INCOMING, "Incoming calls", NotificationManager.IMPORTANCE_HIGH).apply {
                    setSound(null, null)
                    enableVibration(false)
                    lockscreenVisibility = Notification.VISIBILITY_PUBLIC
                },
            )
            nm.createNotificationChannel(
                NotificationChannel(ONGOING, "Ongoing calls", NotificationManager.IMPORTANCE_LOW).apply {
                    setSound(null, null)
                    lockscreenVisibility = Notification.VISIBILITY_PUBLIC
                },
            )
        }

        @Suppress("DEPRECATION") // Call.getState() before API 31; minSdk is 29.
        private fun handle(c: Call): String? = safely(null) { c.details.handle?.schemeSpecificPart }

        /** The contact's name for [number], if the owner allowed contacts. */
        fun contactName(c: Context, number: String): String? {
            if (number.isEmpty() || c.checkSelfPermission(Manifest.permission.READ_CONTACTS) != PackageManager.PERMISSION_GRANTED) return null
            return safely(null) {
                val uri = Uri.withAppendedPath(ContactsContract.PhoneLookup.CONTENT_FILTER_URI, Uri.encode(number))
                c.contentResolver.query(uri, arrayOf(ContactsContract.PhoneLookup.DISPLAY_NAME), null, null, null)?.use {
                    if (it.moveToFirst()) it.getString(0) else null
                }
            }
        }

        private fun identify(c: Context, call: Call): Info {
            val number = handle(call) ?: ""
            val incoming = call.details.callDirection == Call.Details.DIRECTION_INCOMING
            return Info(incoming, number, contactName(c, number), null, 0.0).also { refreshLabel(c, it) }
        }

        /** The remembered Jev verdict (or the instant rules guess) for an incoming call from a non-contact. */
        private fun refreshLabel(c: Context, i: Info) {
            if (!i.incoming || i.name != null || i.number.isEmpty()) return
            safely(Unit) {
                val req = JSONObject().put("number", i.number).put("lang", Locale.getDefault().language)
                val r = unwrap(Native.dialer(c.filesDir.absolutePath, "label", req.toString()))
                i.jev = r.optString("title") + "\n" + r.optString("text")
                i.spam = r.optDouble("spam", 0.0)
            }
        }

        /** The screen/notification model: Rust's, or [CallPlan.fallback] when Rust cannot answer. */
        fun view(c: Context, list: List<Call> = ArrayList(calls)): CallPlan.View {
            val a = audio
            return safely<CallPlan.View?>(null) {
                CallViews.rust(
                    list.map { call ->
                        val i = info[call]?.also { refreshLabel(c, it) }
                        CallViews.Live(call.state, handle(call), call.details.can(Call.Details.CAPABILITY_HOLD), i?.name, i?.jev, i?.spam ?: 0.0)
                    },
                    a,
                )
            } ?: CallPlan.fallback(list.map { CallPlan.CallState(it.state, handle(it), safely(false) { it.details.can(Call.Details.CAPABILITY_HOLD) }) }, a.muted, a.route, a.mask)
        }

        fun answerRinging() {
            calls.firstOrNull { it.state == Call.STATE_RINGING }?.answer(0)
        }

        fun declineRinging() {
            calls.firstOrNull { it.state == Call.STATE_RINGING }?.reject(false, null)
        }

        /** Ends the connected (or dialing) call; with only a held call, that one. */
        fun hangUp() {
            val c = calls.firstOrNull { it.state == Call.STATE_ACTIVE || it.state == Call.STATE_DIALING || it.state == Call.STATE_CONNECTING }
                ?: calls.firstOrNull { it.state != Call.STATE_RINGING }
            c?.disconnect()
        }

        /** Call waiting: end the current call, then answer the one that is ringing. */
        fun endAndAnswer() {
            val ringing = calls.firstOrNull { it.state == Call.STATE_RINGING } ?: return
            calls.firstOrNull { it !== ringing && it.state != Call.STATE_RINGING }?.disconnect()
            ringing.answer(0)
        }
    }
}
