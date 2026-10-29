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
import android.widget.Switch
import java.util.TimeZone
import org.json.JSONArray
import org.json.JSONObject
import tech.getsecretary.design.LucinaTokens.TextSize
import tech.getsecretary.design.LucinaTokens.TextWeight
import tech.getsecretary.design.ValueRow

/**
 * "Caller screening": the call-screening role, the label and auto-reject switches with their limits, the permissions
 * the guards need, the recent rejections (with Allow & call back) and the last screened call. The rule itself runs in
 * Rust (`reject_decision`); this screen edits its settings, which Rust validates.
 */
class CallerScreenActivity : Ui() {
    private lateinit var role: ValueRow
    private lateinit var enable: View
    private lateinit var label: Switch
    private lateinit var reject: Switch
    private lateinit var spamMin: EditText
    private lateinit var pickUpMax: EditText
    private lateinit var orderSpamMin: EditText
    private lateinit var notifications: LinearLayout
    private lateinit var contacts: LinearLayout
    private lateinit var callLog: LinearLayout
    private lateinit var paused: android.widget.TextView
    private lateinit var rejections: LinearLayout
    private lateinit var last: ValueRow

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        screen("Caller screening")
        role = ui.valueRow("Screening role")
        body.addView(ui.group(role.view))
        enable = ui.primaryButton("Turn on call screening") { requestRole() }.apply { layoutParams = ui.lp(top = 12f) }
        body.addView(enable)
        body.addView(ui.footnote("Jev tells you who is probably calling, on a notification. Every call rings unless the rule below turns it away."))

        body.addView(ui.sectionHeader("Auto-reject"))
        val (rejectRow, rejectSwitch) = ui.toggleRow("Auto-reject likely spam") { change(JSONObject().put("reject", JSONObject().put("enabled", it))) }
        val (labelRow, labelSwitch) = ui.toggleRow("Show who is calling") { change(JSONObject().put("label", it)) }
        reject = rejectSwitch
        label = labelSwitch
        body.addView(ui.group(rejectRow, labelRow))
        paused = ui.footnote("").apply { setTextColor(ui.c.negative) }
        body.addView(paused)
        spamMin = limit()
        pickUpMax = limit()
        orderSpamMin = limit()
        body.addView(ui.group(field("Reject when spam is at least", spamMin), field("or pick-up chance is at most", pickUpMax), field("During an order, only when spam is at least", orderSpamMin)))
        body.addView(ui.secondaryButton("Save limits") { saveLimits() }.apply { layoutParams = ui.lp(top = 12f) })
        body.addView(ui.footnote(
            "On by default; limits are between 0 and 1. A call is never rejected if the number is in your contacts, you called it or answered it before, " +
                "you allowed it, it is an emergency or short number, a check could not run (a permission is missing), or Jev did not answer in time. " +
                "A rejected call stays in the call log; you get a quiet note with Allow & call back.",
        ))

        body.addView(ui.sectionHeader("Permissions"))
        notifications = ui.actionRow("Allow notifications") {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_PERMISSION)
        }.view
        contacts = ui.actionRow("Allow contacts (never reject a contact)") {
            requestPermissions(arrayOf(Manifest.permission.READ_CONTACTS), REQUEST_PERMISSION)
        }.view
        callLog = ui.actionRow("Allow call log (never reject who you called)") {
            requestPermissions(arrayOf(Manifest.permission.READ_CALL_LOG, Manifest.permission.WRITE_CALL_LOG), REQUEST_PERMISSION)
        }.view
        body.addView(ui.group(notifications, contacts, callLog))
        body.addView(ui.footnote("Contacts and the call log are only read on this phone to protect those calls. Their names are not sent anywhere."))

        body.addView(ui.sectionHeader("Rejected calls"))
        rejections = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; layoutParams = ui.lp() }
        body.addView(rejections)

        body.addView(ui.sectionHeader("Lately"))
        last = ui.valueRow("Last screened call")
        body.addView(ui.group(last.view))
        refresh()
    }

    private fun limit() = ui.pillInput("0.00", InputType.TYPE_CLASS_NUMBER or InputType.TYPE_NUMBER_FLAG_DECIMAL, TextSize.body).apply {
        gravity = Gravity.END
        layoutParams = ui.lp(w = ui.dp(90f))
    }

    private fun field(text: String, input: EditText) = ui.row().apply {
        addView(ui.text(text).apply { layoutParams = ui.lp(w = 0, weight = 1f) })
        addView(input)
    }

    override fun onResume() {
        super.onResume()
        refresh()
        // Queued reports (rejections, allowed numbers) go out whenever the screen is opened.
        val cookie = safely("") { Calls.cookie() }
        val dir = filesDir.absolutePath
        Calls.io.execute { safely(Unit) { Native.callerFlush(dir, cookie) } }
    }

    private fun held(): Boolean {
        val roles = getSystemService(RoleManager::class.java)
        return roles.isRoleAvailable(RoleManager.ROLE_CALL_SCREENING) && roles.isRoleHeld(RoleManager.ROLE_CALL_SCREENING)
    }

    private fun requestRole() {
        val roles = getSystemService(RoleManager::class.java)
        if (!roles.isRoleAvailable(RoleManager.ROLE_CALL_SCREENING)) {
            notice("Call screening is not available on this phone.")
        } else if (roles.isRoleHeld(RoleManager.ROLE_CALL_SCREENING)) {
            refresh()
        } else {
            startActivityForResult(roles.createRequestRoleIntent(RoleManager.ROLE_CALL_SCREENING), REQUEST_ROLE)
        }
    }

    private fun change(patch: JSONObject): Boolean =
        try {
            show(unwrap(Native.callerSettings(filesDir.absolutePath, patch.toString())))
            true
        } catch (e: Throwable) {
            notice(e.message)
            refresh()
            false
        }

    private fun saveLimits() {
        fun num(e: EditText): Any = e.text.toString().trim().replace(',', '.').toDoubleOrNull() ?: "invalid"
        val saved = change(JSONObject().put("reject", JSONObject()
            .put("spamMin", num(spamMin)).put("pickUpMax", num(pickUpMax)).put("orderSpamMin", num(orderSpamMin))))
        if (saved) notice("Saved.")
    }

    private fun refresh() {
        try {
            show(unwrap(Native.callerState(filesDir.absolutePath)))
        } catch (e: Throwable) {
            notice(e.message)
        }
        loadRejections()
    }

    private fun granted(p: String) = checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED

    private fun show(state: JSONObject) {
        val on = held()
        role.set(if (on) "On" else "Off", if (on) ui.c.positive else ui.c.negative)
        enable.visibility = if (on) View.GONE else View.VISIBLE
        val settings = state.getJSONObject("settings")
        val r = settings.getJSONObject("reject")
        label.isChecked = settings.optBoolean("label", true)
        reject.isChecked = r.optBoolean("enabled")
        for ((e, k) in listOf(spamMin to "spamMin", pickUpMax to "pickUpMax", orderSpamMin to "orderSpamMin")) {
            if (!e.hasFocus()) e.setText(String.format(java.util.Locale.US, "%.2f", r.optDouble(k)))
        }
        val contactsOk = granted(Manifest.permission.READ_CONTACTS)
        val logOk = granted(Manifest.permission.READ_CALL_LOG)
        paused.text = if (r.optBoolean("enabled") && !(contactsOk && logOk)) {
            "Auto-reject is paused: allow contacts and the call log below. Until then no call is rejected."
        } else ""
        paused.visibility = if (paused.text.isEmpty()) View.GONE else View.VISIBLE
        notifications.visibility = if (Build.VERSION.SDK_INT >= 33 && !granted(Manifest.permission.POST_NOTIFICATIONS)) View.VISIBLE else View.GONE
        contacts.visibility = if (contactsOk) View.GONE else View.VISIBLE
        callLog.visibility = if (logOk) View.GONE else View.VISIBLE
        last.set(state.optString("lastLine"))
    }

    private fun loadRejections() {
        val dir = filesDir.absolutePath
        val tz = TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 60000
        Calls.io.execute {
            val rows = safely<JSONArray?>(null) {
                unwrap(Native.dialer(dir, "rejections", JSONObject().put("tzOffsetMin", tz).toString())).getJSONArray("rows")
            }
            runOnUiThread { if (!isDestroyed) showRejections(rows) }
        }
    }

    private fun showRejections(rows: JSONArray?) {
        rejections.removeAllViews()
        if (rows == null || rows.length() == 0) {
            rejections.addView(ui.footnote("Nothing rejected yet."))
            return
        }
        val items = ArrayList<View>()
        for (i in 0 until minOf(rows.length(), 10)) {
            val r = rows.getJSONObject(i)
            val number = r.getString("number")
            val column = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                addView(ui.text(r.getString("line"), TextSize.body, TextWeight.body, ui.c.text))
                addView(ui.text(r.getString("when"), TextSize.footnote, TextWeight.footnote, ui.c.textSecondary))
                if (r.optBoolean("allowed")) {
                    addView(ui.text("Allowed", TextSize.footnote, TextWeight.footnote, ui.c.positive))
                } else {
                    addView(ui.pair(
                        ui.primaryButton("Allow & call back") { allow(number, true) },
                        ui.secondaryButton("Allow") { allow(number, false) },
                    ).apply { layoutParams = ui.lp(top = 8f) })
                }
            }
            items.add(ui.row().apply { addView(column, ui.lp(w = 0, weight = 1f)) })
        }
        rejections.addView(ui.group(*items.toTypedArray()))
    }

    /** On his tap only: allow the number, report it, and optionally call it through Telecom. */
    private fun allow(number: String, callBack: Boolean) {
        val cookie = safely("") { Calls.cookie() }
        val app = applicationContext
        Calls.io.execute {
            val problem = RejectNotice.allow(app, number, callBack, cookie)
            runOnUiThread {
                if (problem != null) notice(problem)
                loadRejections()
            }
        }
    }

    @Deprecated("Activity result API; there is no AndroidX here.")
    override fun onActivityResult(request: Int, result: Int, data: Intent?) {
        super.onActivityResult(request, result, data)
        if (request == REQUEST_ROLE) refresh()
    }

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        refresh()
    }

    private companion object {
        const val REQUEST_ROLE = 20
        const val REQUEST_PERMISSION = 21
    }
}
