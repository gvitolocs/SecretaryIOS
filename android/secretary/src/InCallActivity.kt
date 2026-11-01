package tech.getsecretary.secretary

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.telecom.Call
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import tech.getsecretary.design.LucinaTokens.TextSize
import tech.getsecretary.design.LucinaTokens.TextWeight
import tech.getsecretary.design.LucinaUi

/**
 * The in-call screen: rows, buttons and controls come from [LucinaCallService.view] (Rust's `callView`, or the
 * plain-Kotlin fallback), so it renders even if the native library is unavailable. It shows live over the lock
 * screen and turns the screen on. Every button acts only when tapped.
 */
class InCallActivity : Ui() {
    private val main = Handler(Looper.getMainLooper())
    private var keypad = false
    private val typed = StringBuilder()
    private val finishLater = Runnable { finish() }
    private val changed = object : BroadcastReceiver() {
        override fun onReceive(c: Context, i: Intent) {
            render()
        }
    }

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        setShowWhenLocked(true)
        setTurnScreenOn(true)
        val filter = IntentFilter(LucinaCallService.CHANGED)
        if (Build.VERSION.SDK_INT >= 33) registerReceiver(changed, filter, Context.RECEIVER_NOT_EXPORTED) else registerReceiver(changed, filter)
        render()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        render()
    }

    @Suppress("DEPRECATION") // InCallService.setAudioRoute: the newer endpoint API needs API 34.
    private fun render() {
        main.removeCallbacks(finishLater)
        ui = LucinaUi(this)
        body = ui.screen(this)
        val calls = ArrayList(LucinaCallService.calls)
        if (calls.isEmpty()) {
            body.addView(centered("Phone", TextSize.subheadline, TextWeight.subheadline, ui.c.textSecondary, top = 72f))
            body.addView(centered("Call ended", TextSize.largeTitle, TextWeight.largeTitle, ui.c.text, top = 4f))
            body.addView(spacer())
            body.addView(ui.primaryButton("View transcript") { startActivity(Intent(this, TranscriptActivity::class.java)) })
            body.addView(ui.secondaryButton("Close") { finish() }.apply { layoutParams = ui.lp(top = 12f) })
            keypad = false
            typed.setLength(0)
            main.postDelayed(finishLater, 4000)
            return
        }
        val v = LucinaCallService.view(this, calls)
        val ends = ArrayList<View>()
        val holds = ArrayList<View>()
        for (i in calls.indices) {
            val c = calls[i]
            val row = v.rows.getOrNull(i) ?: continue
            body.addView(centered(row.label, TextSize.subheadline, TextWeight.subheadline, ui.c.textSecondary, top = if (i == 0) 48f else 24f))
            body.addView(centered(row.peer, if (i == 0) TextSize.largeTitle else TextSize.title, TextWeight.largeTitle, ui.c.text, top = 4f))
            row.number?.let { body.addView(centered(it, TextSize.subheadline, TextWeight.subheadline, ui.c.textSecondary, top = 2f)) }
            row.jev?.let {
                val colour = if (row.warn) ui.c.negative else ui.c.accent
                body.addView(centered(it, TextSize.subheadline, TextWeight.subheadline, colour, top = 10f))
            }
            var answer: View? = null
            var decline: View? = null
            var endAnswer: View? = null
            for (a in row.actions) {
                when (a) {
                    "answer" -> answer = ui.filledButton("Answer", ui.c.positive) { act { c.answer(0) } }
                    "decline" -> decline = ui.filledButton("Decline", ui.c.negative) { act { c.reject(false, null) } }
                    "end_answer" -> endAnswer = ui.secondaryButton("End current call & answer") { act { LucinaCallService.endAndAnswer() } }
                    "end" -> ends.add(
                        ui.filledButton(if (calls.size > 1) "End · ${row.peer}" else "End call", ui.c.negative) { act { c.disconnect() } }
                            .apply { layoutParams = ui.lp(top = 12f) },
                    )
                    "resume" -> holds.add(round(Glyph.Kind.PLAY, if (calls.size > 1) "Resume ${row.peer}" else "Resume") { act { c.unhold() } })
                    "hold" -> holds.add(round(Glyph.Kind.PAUSE, if (calls.size > 1) "Hold ${row.peer}" else "Hold") { act { c.hold() } })
                }
            }
            if (answer != null && decline != null) {
                body.addView(ui.pair(decline, answer).apply { layoutParams = ui.lp(top = 24f) })
            } else {
                answer?.let { body.addView(it.apply { layoutParams = ui.lp(top = 24f) }) }
                decline?.let { body.addView(it.apply { layoutParams = ui.lp(top = 24f) }) }
            }
            endAnswer?.let { body.addView(it.apply { layoutParams = ui.lp(top = 12f) }) }
        }
        val ctl = v.controls
        val buttons = ArrayList<View>()
        buttons.add(round(if (ctl.muted) Glyph.Kind.MIC_OFF else Glyph.Kind.MIC, if (ctl.muted) "Unmute" else "Mute", on = ctl.muted) {
            act { LucinaCallService.live?.setMuted(!ctl.muted) }
        })
        for (r in ctl.routes) {
            val kind = when (r.route) {
                CallPlan.SPEAKER -> Glyph.Kind.SPEAKER
                CallPlan.EARPIECE -> Glyph.Kind.EARPIECE
                else -> null
            }
            buttons.add(
                if (kind != null) round(kind, r.name, on = r.on) { act { LucinaCallService.live?.setAudioRoute(r.route) } }
                else textRound(if (r.route == CallPlan.BLUETOOTH) "BT" else "HS", r.name, on = r.on) { act { LucinaCallService.live?.setAudioRoute(r.route) } },
            )
        }
        body.addView(ui.spread(*buttons.take(4).toTypedArray()).apply { layoutParams = ui.lp(top = 36f) })
        if (buttons.size > 4) body.addView(ui.spread(*buttons.drop(4).toTypedArray()).apply { layoutParams = ui.lp(top = 16f) })
        val extras = ArrayList<View>()
        extras.addAll(holds)
        extras.add(textRound("123", "Keypad", on = keypad) { keypad = !keypad; render() })
        extras.add(textRound("+", "Add call") { startActivity(Intent(this, CallsActivity::class.java)) })
        body.addView(ui.spread(*extras.take(4).toTypedArray()).apply { layoutParams = ui.lp(top = 16f) })
        if (extras.size > 4) body.addView(ui.spread(*extras.drop(4).toTypedArray()).apply { layoutParams = ui.lp(top = 16f) })
        if (keypad) addKeypad()
        body.addView(spacer())
        for (end in ends) body.addView(end)
    }

    /** Runs a call control; a failing one must not take the screen down. */
    private inline fun act(f: () -> Unit) {
        safely(Unit) { f() }
    }

    private fun addKeypad() {
        body.addView(centered(typed.toString().ifEmpty { " " }, TextSize.title2, TextWeight.largeTitle, ui.c.text, top = 20f))
        val keys = arrayOf("1" to "", "2" to "ABC", "3" to "DEF", "4" to "GHI", "5" to "JKL", "6" to "MNO",
            "7" to "PQRS", "8" to "TUV", "9" to "WXYZ", "*" to "", "0" to "+", "#" to "")
        for (row in 0 until 4) {
            val views = (0 until 3).map { col ->
                val (digit, letters) = keys[row * 3 + col]
                ui.keypadKey(digit, letters, size = 64f) { tone(digit[0]) }
            }
            body.addView(ui.spread(*views.toTypedArray()).apply { layoutParams = ui.lp(top = 10f) })
        }
    }

    /** One DTMF tone on the connected call. */
    private fun tone(d: Char) {
        val c: Call = LucinaCallService.calls.firstOrNull { it.state == Call.STATE_ACTIVE } ?: return
        safely(Unit) {
            c.playDtmfTone(d)
            main.postDelayed({ safely(Unit) { c.stopDtmfTone() } }, 160)
        }
        typed.append(d)
        if (typed.length > 30) typed.deleteCharAt(0)
        render()
    }

    private fun centered(s: String, size: Float, weight: Int, color: Int, top: Float) =
        ui.text(s, size, weight, color).apply {
            gravity = Gravity.CENTER
            layoutParams = ui.lp(top = top)
        }

    /** Takes the free height, so the hang-up pill sits at the bottom of a short page. */
    private fun spacer() = View(this).apply { layoutParams = ui.lp(h = 0, weight = 1f).apply { topMargin = ui.dp(24f) } }

    private fun round(kind: Glyph.Kind, label: String, on: Boolean = false, onClick: () -> Unit) =
        ui.labeled(ui.circle(icon(kind, if (on) ui.c.onAccent else ui.c.text, 28f), 68f, if (on) ui.c.accentFill else ui.c.cardRaised, onClick), label)

    private fun textRound(glyph: String, label: String, on: Boolean = false, onClick: () -> Unit) =
        ui.labeled(
            ui.circle(
                ui.text(glyph, TextSize.headline, TextWeight.headline, if (on) ui.c.onAccent else ui.c.text),
                68f, if (on) ui.c.accentFill else ui.c.cardRaised, onClick,
            ),
            label,
        )

    override fun onDestroy() {
        main.removeCallbacks(finishLater)
        safely(Unit) { unregisterReceiver(changed) }
        super.onDestroy()
    }
}
