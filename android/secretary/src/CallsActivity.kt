package tech.getsecretary.secretary

import android.Manifest
import android.app.role.RoleManager
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.widget.EditText
import android.widget.LinearLayout
import org.json.JSONObject
import tech.getsecretary.design.LucinaTokens

/** The dialer (DIAL intents), the voice conversation and the transcript list. */
class CallsActivity : Ui() {
    private lateinit var number: EditText

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        screen("Calls")
        number = ui.pillInput("Phone number", InputType.TYPE_CLASS_PHONE, LucinaTokens.TextSize.title2).apply {
            gravity = Gravity.CENTER
            showSoftInputOnFocus = false // the keypad below types; long-press still pastes
            layoutParams = ui.lp(top = 8f, bottom = 20f)
        }
        body.addView(number)
        intent.data?.let { number.setText(it.schemeSpecificPart) }
        val keys = arrayOf("1" to "", "2" to "ABC", "3" to "DEF", "4" to "GHI", "5" to "JKL", "6" to "MNO",
            "7" to "PQRS", "8" to "TUV", "9" to "WXYZ", "*" to "", "0" to "+", "#" to "")
        for (row in 0 until 4) {
            val views = (0 until 3).map { col ->
                val (digit, letters) = keys[row * 3 + col]
                ui.keypadKey(digit, letters, onLong = if (digit == "0") ({ type("+") }) else null) { type(digit) }
            }
            body.addView(ui.spread(*views.toTypedArray()).apply { layoutParams = ui.lp(bottom = 14f) })
        }
        val call = ui.filledButton("Call", ui.c.positive) { dial() }
        val erase = ui.circle(icon(Glyph.Kind.BACKSPACE, ui.c.textSecondary, 26f), 52f, ui.c.bg) { erase() }
        erase.setOnLongClickListener { number.setText(""); true }
        body.addView(LinearLayout(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            addView(View(context), ui.lp(w = ui.dp(52f), h = 1))
            addView(call, ui.lp(w = 0, weight = 1f).apply { leftMargin = ui.dp(16f); rightMargin = ui.dp(16f) })
            addView(erase)
            layoutParams = ui.lp(top = 4f)
        })

        body.addView(ui.sectionHeader("Lucina"))
        body.addView(ui.group(
            ui.valueRow("Talk to Lucina", "Internet", chevron = true) { startActivity(Intent(this, VoiceActivity::class.java)) }.view,
            ui.valueRow("Call history & transcripts", chevron = true) { startActivity(Intent(this, TranscriptActivity::class.java)) }.view,
        ))
        body.addView(ui.sectionHeader("Phone"))
        body.addView(ui.group(
            ui.actionRow("Set up SIM call controls") {
                val roles = getSystemService(RoleManager::class.java)
                if (roles.isRoleAvailable(RoleManager.ROLE_DIALER) && !roles.isRoleHeld(RoleManager.ROLE_DIALER)) {
                    startActivityForResult(roles.createRequestRoleIntent(RoleManager.ROLE_DIALER), REQUEST_ROLE)
                } else {
                    notice("Secretary already handles phone calls.")
                    notifications()
                }
            }.view,
        ))
        body.addView(ui.footnote(
            "SIM calls use Android’s phone role. Recording both sides is not available to a normal app. " +
                "Import an existing call recording to transcribe it. Internet conversations here are with Lucina; " +
                "other VoIP services are not connected.",
        ))
    }

    /** Types at the cursor when the field is focused (replacing a selection), otherwise at the end. */
    private fun type(s: String) {
        val t = number.text
        val start = minOf(number.selectionStart, number.selectionEnd)
        val end = maxOf(number.selectionStart, number.selectionEnd)
        if (!number.hasFocus() || start < 0) t.append(s) else t.replace(start, end, s)
    }

    /** Deletes the selection or the character before the cursor (the last one when unfocused). */
    private fun erase() {
        val t = number.text
        val start = minOf(number.selectionStart, number.selectionEnd)
        val end = maxOf(number.selectionStart, number.selectionEnd)
        when {
            !number.hasFocus() || start < 0 -> if (t.isNotEmpty()) t.delete(t.length - 1, t.length)
            start != end -> t.delete(start, end)
            start > 0 -> t.delete(start - 1, start)
        }
    }

    private fun notifications() {
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_NOTIFICATIONS)
        }
    }

    @Deprecated("Activity result API; there is no AndroidX here.")
    override fun onActivityResult(request: Int, result: Int, data: Intent?) {
        super.onActivityResult(request, result, data)
        if (request == REQUEST_ROLE && result == RESULT_OK) notifications()
    }

    /** Rust validates the number and checks CALL_PHONE before Telecom sees it. */
    private fun dial() {
        val n = number.text.toString().trim()
        if (n.isEmpty()) {
            notice("Enter a phone number.")
            return
        }
        val result = try {
            JSONObject(Native.phoneCall(this, n, false))
        } catch (e: Exception) {
            notice("Phone function failed.")
            return
        }
        if (result.optString("status") == "needs_permission") {
            requestPermissions(arrayOf(Manifest.permission.CALL_PHONE), REQUEST_CALL)
            return
        }
        notice(result.optString("message", result.optString("status")))
    }

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        if (r == REQUEST_CALL && g.isNotEmpty() && g[0] == PackageManager.PERMISSION_GRANTED) {
            notice("Call permission enabled. Tap Call when ready.")
        }
    }

    private companion object {
        const val REQUEST_ROLE = 1
        const val REQUEST_CALL = 2
        const val REQUEST_NOTIFICATIONS = 4
    }
}
