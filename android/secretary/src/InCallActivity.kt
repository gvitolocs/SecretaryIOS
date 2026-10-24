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
import android.widget.EditText
import org.json.JSONObject

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
        screen("Lucina · Phone")
        val calls = ArrayList(LucinaCallService.calls)
        if (calls.isEmpty()) {
            text("Call ended", 22)
            button("View transcript") { startActivity(Intent(this, TranscriptActivity::class.java)) }
            return
        }
        val rows = try {
            LucinaCallService.view(calls).getJSONArray("calls")
        } catch (e: Exception) {
            null
        }
        for (i in calls.indices) {
            val c = calls[i]
            val row = rows?.optJSONObject(i) ?: JSONObject()
            text(row.optString("peer", "Phone call"), 24)
            text(row.optString("label"), 15)
            val actions = row.optJSONArray("actions") ?: continue
            for (a in 0 until actions.length()) {
                when (actions.optString(a)) {
                    "answer" -> button("Answer") { c.answer(c.details.videoState) }
                    "decline" -> button("Decline") { c.reject(false, null) }
                    "end" -> button("End call") { c.disconnect() }
                    "resume" -> button("Resume") { c.unhold() }
                    "hold" -> button("Hold") { c.hold() }
                }
            }
        }
        button("Mute microphone") { LucinaCallService.live?.setMuted(true) }
        button("Unmute") { LucinaCallService.live?.setMuted(false) }
        button("Speaker") { LucinaCallService.live?.setAudioRoute(CallAudioState.ROUTE_SPEAKER) }
        button("Earpiece") { LucinaCallService.live?.setAudioRoute(CallAudioState.ROUTE_EARPIECE) }
        val tones = EditText(this)
        tones.hint = "Keypad digits"
        tones.inputType = InputType.TYPE_CLASS_PHONE
        body.addView(tones)
        button("Send keypad digits") { sendTones(tones.text.toString()) }
    }

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
