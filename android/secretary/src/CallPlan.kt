package tech.getsecretary.secretary

/** Run [f]; any failure, including a missing native library, gives [default]. */
inline fun <T> safely(default: T, f: () -> T): T = try {
    f()
} catch (t: Throwable) {
    default
}

/**
 * The in-call screen's models and the plain-Kotlin plan used when Rust's `callView` cannot answer (the
 * library failed to load, a JNI error). It is the same table as `ui::call_view_with`, kept small on purpose:
 * an emergency call must always get a screen with End, Mute and Speaker.
 */
object CallPlan {
    // android.telecom.Call states
    const val NEW = 0
    const val DIALING = 1
    const val RINGING = 2
    const val HOLDING = 3
    const val ACTIVE = 4
    const val DISCONNECTED = 7
    const val CONNECTING = 9
    // android.telecom.CallAudioState routes
    const val EARPIECE = 1
    const val BLUETOOTH = 2
    const val WIRED_HEADSET = 4
    const val SPEAKER = 8

    data class CallState(val state: Int, val handle: String?, val canHold: Boolean)

    data class Row(
        val peer: String,
        val number: String?,
        val label: String,
        val actions: List<String>,
        val jev: String?,
        val warn: Boolean,
    )

    data class Route(val route: Int, val name: String, val on: Boolean)

    data class Controls(val muted: Boolean, val routes: List<Route>, val proximity: Boolean)

    data class Notice(val index: Int, val title: String, val text: String, val fullScreen: Boolean, val sub: String?, val actions: List<String>)

    data class View(val rows: List<Row>, val notice: Notice?, val controls: Controls)

    fun label(state: Int): String = when (state) {
        RINGING -> "Incoming"
        ACTIVE -> "Connected"
        DIALING -> "Dialing"
        HOLDING -> "On hold"
        DISCONNECTED -> "Disconnected"
        else -> "Connecting"
    }

    fun peer(handle: String?): String {
        val s = (handle ?: "").filter { !it.isISOControl() }.take(120)
        return if (s.isBlank()) "Private number" else s
    }

    fun routes(muted: Boolean, route: Int, mask: Int): List<Route> {
        val m = if (mask == 0) EARPIECE or SPEAKER else mask
        return listOf(EARPIECE to "Earpiece", WIRED_HEADSET to "Headset", BLUETOOTH to "Bluetooth", SPEAKER to "Speaker")
            .filter { m and it.first != 0 }
            .map { Route(it.first, it.second, it.first == route) }
    }

    fun fallback(calls: List<CallState>, muted: Boolean, route: Int, mask: Int): View {
        val rows = calls.mapIndexed { i, c ->
            val othersLive = calls.withIndex().any { (j, o) -> j != i && (o.state == ACTIVE || o.state == HOLDING || o.state == DIALING) }
            val actions = when {
                c.state == RINGING -> if (othersLive) listOf("answer", "end_answer", "decline") else listOf("answer", "decline")
                c.state == HOLDING -> listOf("end", "resume")
                c.canHold -> listOf("end", "hold")
                else -> listOf("end")
            }
            Row(peer(c.handle), null, label(c.state), actions, null, false)
        }
        val index = calls.indexOfFirst { it.state == RINGING }.coerceAtLeast(0)
        val ringing = calls.isNotEmpty() && calls[index].state == RINGING
        val notice = if (calls.isEmpty()) null else Notice(
            index,
            if (ringing) "Incoming call" else "Phone call",
            rows[index].peer,
            ringing || calls[index].state == DIALING,
            null,
            if (ringing) listOf("answer", "decline") else listOf("hangup"),
        )
        val anyRinging = calls.any { it.state == RINGING }
        val atEar = calls.any { it.state == ACTIVE || it.state == DIALING || it.state == NEW || it.state == CONNECTING }
        return View(rows, notice, Controls(muted, routes(muted, route, mask), atEar && !anyRinging && route == EARPIECE))
    }
}
