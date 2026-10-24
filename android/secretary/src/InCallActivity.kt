package tech.getsecretary.secretary

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.telecom.Call
import android.telecom.CallAudioState
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import org.json.JSONObject
import tech.getsecretary.design.LucinaTokens.TextSize
import tech.getsecretary.design.LucinaTokens.TextWeight
import tech.getsecretary.design.LucinaUi

/** The in-call screen: rows and buttons come from Rust's `callView`, the keypad from `dtmf`. */
class InCallActivity : Ui() {
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

    @Suppress("DEPRECATION") // InCallService.setAudioRoute: the newer endpoint API needs API 34.
    private fun render() {
        ui = LucinaUi(this)
        body = ui.screen(this)
        val calls = ArrayList(LucinaCallService.calls)
        if (calls.isEmpty()) {
            body.addView(centered("Phone", TextSize.subheadline, TextWeight.subheadline, ui.c.textSecondary, top = 72f))
            body.addView(centered("Call ended", TextSize.largeTitle, TextWeight.largeTitle, ui.c.text, top = 4f))
            body.addView(spacer())
            body.addView(ui.primaryButton("View transcript") { startActivity(Intent(this, TranscriptActivity::class.java)) })
            return
        }
        val rows = try {
            LucinaCallService.view(calls).getJSONArray("calls")
        } catch (e: Exception) {
            null
        }
        val ends = ArrayList<View>()
        val holds = ArrayList<View>()
        for (i in calls.indices) {
            val c = calls[i]
            val row = rows?.optJSONObject(i) ?: JSONObject()
            val peer = row.optString("peer", "Phone call")
            body.addView(centered(row.optString("label"), TextSize.subheadline, TextWeight.subheadline, ui.c.textSecondary,
                top = if (i == 0) 56f else 28f))
            body.addView(centered(peer, if (i == 0) TextSize.largeTitle else TextSize.title, TextWeight.largeTitle, ui.c.text, top = 4f))
            val actions = row.optJSONArray("actions") ?: continue
            var answer: View? = null
            var decline: View? = null
            for (a in 0 until actions.length()) {
                when (actions.optString(a)) {
                    "answer" -> answer = ui.filledButton("Answer", ui.c.positive) { c.answer(c.details.videoState) }
                    "decline" -> decline = ui.filledButton("Decline", ui.c.negative) { c.reject(false, null) }
                    "end" -> ends.add(ui.filledButton(if (calls.size > 1) "End · $peer" else "End call", ui.c.negative) { c.disconnect() }
                        .apply { layoutParams = ui.lp(top = 12f) })
                    "resume" -> holds.add(round(Glyph.Kind.PLAY, if (calls.size > 1) "Resume $peer" else "Resume") { c.unhold() })
                    "hold" -> holds.add(round(Glyph.Kind.PAUSE, if (calls.size > 1) "Hold $peer" else "Hold") { c.hold() })
                }
            }
            if (answer != null && decline != null) {
                body.addView(ui.pair(decline, answer).apply { layoutParams = ui.lp(top = 28f) })
            } else {
                answer?.let { body.addView(it.apply { layoutParams = ui.lp(top = 28f) }) }
                decline?.let { body.addView(it.apply { layoutParams = ui.lp(top = 28f) }) }
            }
        }
        body.addView(ui.spread(
            round(Glyph.Kind.MIC_OFF, "Mute") { LucinaCallService.live?.setMuted(true) },
            round(Glyph.Kind.MIC, "Unmute") { LucinaCallService.live?.setMuted(false) },
            round(Glyph.Kind.SPEAKER, "Speaker") { LucinaCallService.live?.setAudioRoute(CallAudioState.ROUTE_SPEAKER) },
            round(Glyph.Kind.EARPIECE, "Earpiece") { LucinaCallService.live?.setAudioRoute(CallAudioState.ROUTE_EARPIECE) },
        ).apply { layoutParams = ui.lp(top = 44f) })
        if (holds.isNotEmpty()) body.addView(ui.spread(*holds.toTypedArray()).apply { layoutParams = ui.lp(top = 20f) })
        val tones = ui.pillInput("Keypad digits", InputType.TYPE_CLASS_PHONE)
        body.addView(LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            addView(tones, ui.lp(w = 0, weight = 1f))
            addView(ui.secondaryButton("Send") { sendTones(tones.text.toString()) }, ui.lp(w = -2).apply { leftMargin = ui.dp(12f) })
            layoutParams = ui.lp(top = 28f)
        })
        body.addView(spacer())
        for (end in ends) body.addView(end)
    }

    private fun centered(s: String, size: Float, weight: Int, color: Int, top: Float) =
        ui.text(s, size, weight, color).apply {
            gravity = Gravity.CENTER
            layoutParams = ui.lp(top = top)
        }

    /** Takes the free height, so the hang-up pill sits at the bottom of a short page. */
    private fun spacer() = View(this).apply { layoutParams = ui.lp(h = 0, weight = 1f).apply { topMargin = ui.dp(24f) } }

    private fun round(kind: Glyph.Kind, label: String, onClick: () -> Unit) =
        ui.labeled(ui.circle(icon(kind, ui.c.text, 28f), 68f, ui.c.cardRaised, onClick), label)

    private fun sendTones(digits: String) {
        val c: Call = LucinaCallService.calls.firstOrNull() ?: return
        val tones = try {
            Calls.ui("dtmf", JSONObject().put("digits", digits)).getJSONArray("tones")
        } catch (e: Exception) {
            return
        }
        val h = Handler(mainLooper)
        for (i in 0 until tones.length()) {
            val t = tones.getJSONObject(i)
            val tone = t.getString("tone")[0]
            h.postDelayed({
                c.playDtmfTone(tone)
                h.postDelayed({ c.stopDtmfTone() }, 120)
            }, t.getLong("delayMs"))
        }
    }

    override fun onDestroy() {
        unregisterReceiver(changed)
        super.onDestroy()
    }
}
