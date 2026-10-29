package tech.getsecretary.secretary

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.widget.EditText
import android.widget.LinearLayout
import tech.getsecretary.design.ValueRow
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
                ui.keypadKey(digit, letters, onLong = when (digit) {
                    "0" -> ({ type("+") })
                    "1" -> ({ voicemail() }) // long-press 1 calls the carrier voicemail, as on every phone
                    else -> null
                }) { type(digit) }
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

        body.addView(ui.sectionHeader("Phone"))
        body.addView(ui.group(
            ui.valueRow("Recent calls", chevron = true) { startActivity(Intent(this, CallLogActivity::class.java)) }.view,
            ui.valueRow("Contacts", chevron = true) { startActivity(Intent(this, ContactsActivity::class.java)) }.view,
            ui.valueRow("Voicemail", chevron = true) { voicemail() }.view,
        ))
        body.addView(ui.footnote("Emergency numbers such as 112 are dialed straight through Android, with no checks of ours in between."))
        body.addView(ui.sectionHeader("Lucina"))
        body.addView(ui.group(
            ui.valueRow("Talk to Lucina", "Internet", chevron = true) { startActivity(Intent(this, VoiceActivity::class.java)) }.view,
            ui.valueRow("Call history & transcripts", chevron = true) { startActivity(Intent(this, TranscriptActivity::class.java)) }.view,
            ui.valueRow("Caller screening", chevron = true) { startActivity(Intent(this, CallerScreenActivity::class.java)) }.view,
        ))
        body.addView(ui.sectionHeader("Default phone app"))
        role = ui.valueRow("Secretary is the phone app")
        makeDefault = ui.actionRow("Make Secretary your phone app") { requestRole() }
        val back = ui.actionRow("Use ${Phone.previousDialer(this)?.let { label(it) } ?: "Samsung Phone"} again") { switchBack() }
        body.addView(ui.group(role.view, makeDefault.view, back.view))
        body.addView(ui.footnote(
            "Only you can change the default phone app: Android asks you to confirm. Calls, call waiting, the call log " +
                "and voicemail then run here; Samsung Phone stays installed and the button above hands the role back. " +
                "SIM audio is not available to a normal app, so SIM calls are not recorded; import a recording to transcribe it.",
        ))
        refreshRole()
    }

    private lateinit var role: ValueRow
    private lateinit var makeDefault: ValueRow

    override fun onResume() {
        super.onResume()
        if (::role.isInitialized) refreshRole()
    }

    private fun label(pkg: String): String = try {
        packageManager.getApplicationInfo(pkg, 0).loadLabel(packageManager).toString()
    } catch (e: Exception) {
        "Samsung Phone"
    }

    private fun refreshRole() {
        val held = Phone.isDefaultDialer(this)
        role.set(if (held) "Yes" else "No", if (held) ui.c.positive else ui.c.negative)
        makeDefault.view.visibility = if (held || !Phone.roleAvailable(this)) View.GONE else View.VISIBLE
    }

    private fun requestRole() {
        val intent = Phone.requestRole(this)
        if (intent == null) {
            notice(if (Phone.roleHeld(this)) "Secretary is already the phone app." else "This phone does not offer the phone role.")
        } else {
            startActivityForResult(intent, REQUEST_ROLE)
        }
    }

    private fun switchBack() {
        if (Phone.switchBack(this, REQUEST_BACK) < 0) notice("Pick the Phone app in Default apps.")
    }

    private fun voicemail() {
        if (!Phone.callVoicemail(this)) {
            if (checkSelfPermission(Manifest.permission.CALL_PHONE) != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(arrayOf(Manifest.permission.CALL_PHONE), REQUEST_VOICEMAIL)
            } else {
                notice("No voicemail number is set up on this SIM.")
            }
        }
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
        if (::role.isInitialized) refreshRole()
    }

    /**
     * An emergency number is placed at once through Telecom, with no permission request, no confirmation and no
     * Rust or network on the way (see [DialPlan]). Other numbers need CALL_PHONE, which the phone role grants.
     */
    private fun dial() {
        val n = number.text.toString().trim()
        if (n.isEmpty()) {
            notice("Enter a phone number.")
            return
        }
        val plan = DialPlan.plan(n) { Phone.isEmergency(this, it) }
        when (plan.kind) {
            DialPlan.Kind.INVALID -> notice("Enter a valid phone number.")
            DialPlan.Kind.EMERGENCY -> Phone.dial(this, plan.number)
            DialPlan.Kind.CALL -> {
                if (checkSelfPermission(Manifest.permission.CALL_PHONE) != PackageManager.PERMISSION_GRANTED) {
                    pending = plan.number
                    requestPermissions(arrayOf(Manifest.permission.CALL_PHONE), REQUEST_CALL)
                } else {
                    place(plan.number)
                }
            }
        }
    }

    private var pending: String? = null

    private fun place(n: String) {
        val out = Phone.dial(this, n)
        if (out is DialPlan.Outcome.Failed) notice("Android refused the call. Check the SIM and phone permission.")
    }

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        val granted = g.isNotEmpty() && g[0] == PackageManager.PERMISSION_GRANTED
        if (r == REQUEST_CALL && granted) {
            pending?.let { place(it) } // the number he already typed and tapped Call for
        }
        if (r == REQUEST_VOICEMAIL && granted) voicemail()
        pending = null
    }

    private companion object {
        const val REQUEST_ROLE = 1
        const val REQUEST_CALL = 2
        const val REQUEST_NOTIFICATIONS = 4
        const val REQUEST_BACK = 5
        const val REQUEST_VOICEMAIL = 6
    }
}
