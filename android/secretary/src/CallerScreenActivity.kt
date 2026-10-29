package tech.getsecretary.secretary

import android.Manifest
import android.app.role.RoleManager
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.View
import android.widget.LinearLayout
import android.widget.Switch
import org.json.JSONObject
import tech.getsecretary.design.ValueRow

/** "Caller screening": the call-screening role, the owner's switches, Lucina Maps' courier state and the last call. */
class CallerScreenActivity : Ui() {
    private lateinit var role: ValueRow
    private lateinit var enable: View
    private lateinit var label: Switch
    private lateinit var silence: Switch
    private lateinit var reject: Switch
    private lateinit var notifications: LinearLayout
    private lateinit var contacts: LinearLayout
    private lateinit var courier: ValueRow
    private lateinit var last: ValueRow

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        screen("Caller screening")
        role = ui.valueRow("Screening role")
        body.addView(ui.group(role.view))
        enable = ui.primaryButton("Turn on call screening") { requestRole() }.apply { layoutParams = ui.lp(top = 12f) }
        body.addView(enable)
        body.addView(ui.footnote("Incoming calls are always put through. Jev only tells you who is probably calling, on a notification."))

        body.addView(ui.sectionHeader("What it does"))
        val (labelRow, labelSwitch) = ui.toggleRow("Show who is calling") { change("label", it) }
        val (silenceRow, silenceSwitch) = ui.toggleRow("Silence likely scams") { change("silence", it) }
        val (rejectRow, rejectSwitch) = ui.toggleRow("Reject likely scams") { change("reject", it) }
        label = labelSwitch; silence = silenceSwitch; reject = rejectSwitch
        body.addView(ui.group(labelRow, silenceRow, rejectRow))
        body.addView(ui.footnote(
            "Silence and reject are off until you turn them on. They act only when Jev itself is at least 90% sure " +
                "(never on the quick offline guess, never on a contact). A rejected call still shows in the call log.",
        ))

        body.addView(ui.sectionHeader("Permissions"))
        notifications = ui.actionRow("Allow notifications") {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_NOTIFICATIONS)
        }.view
        contacts = ui.actionRow("Skip my contacts") {
            requestPermissions(arrayOf(Manifest.permission.READ_CONTACTS), REQUEST_CONTACTS)
        }.view
        body.addView(ui.group(notifications, contacts))
        body.addView(ui.footnote("Contacts are only looked up on this phone, to leave your own contacts out. Their names are not sent anywhere."))

        body.addView(ui.sectionHeader("Lately"))
        courier = ui.valueRow("Courier (Lucina Maps)")
        last = ui.valueRow("Last screened call")
        body.addView(ui.group(courier.view, last.view))
        refresh()
    }

    override fun onResume() {
        super.onResume()
        refresh()
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

    private fun change(key: String, on: Boolean) {
        try {
            show(unwrap(Native.callerSettings(filesDir.absolutePath, JSONObject().put(key, on).toString())))
        } catch (e: Exception) {
            notice(e.message)
            refresh()
        }
    }

    private fun refresh() {
        try {
            show(unwrap(Native.callerState(filesDir.absolutePath)))
        } catch (e: Exception) {
            notice(e.message)
        }
    }

    private fun show(state: JSONObject) {
        val on = held()
        role.set(if (on) "On" else "Off", if (on) ui.c.positive else ui.c.negative)
        enable.visibility = if (on) View.GONE else View.VISIBLE
        val settings = state.getJSONObject("settings")
        label.isChecked = settings.optBoolean("label", true)
        silence.isChecked = settings.optBoolean("silence")
        reject.isChecked = settings.optBoolean("reject")
        notifications.visibility = if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) View.VISIBLE else View.GONE
        contacts.visibility = if (checkSelfPermission(Manifest.permission.READ_CONTACTS) == PackageManager.PERMISSION_GRANTED) View.GONE else View.VISIBLE
        courier.set(state.optString("courierLine"))
        last.set(state.optString("lastLine"))
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
        const val REQUEST_NOTIFICATIONS = 21
        const val REQUEST_CONTACTS = 22
    }
}
