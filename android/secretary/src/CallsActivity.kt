package tech.getsecretary.secretary

import android.Manifest
import android.app.role.RoleManager
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.widget.EditText
import org.json.JSONObject

/** The dialer (DIAL intents), the voice conversation and the transcript list. */
class CallsActivity : Ui() {
    private lateinit var number: EditText

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        screen("Lucina · Calls")
        text("Phone calls, conversations with Lucina, and private transcripts.", 15)
        button("Talk to Lucina · internet") { startActivity(Intent(this, VoiceActivity::class.java)) }
        button("Set up SIM call controls") {
            val roles = getSystemService(RoleManager::class.java)
            if (roles.isRoleAvailable(RoleManager.ROLE_DIALER) && !roles.isRoleHeld(RoleManager.ROLE_DIALER)) {
                startActivityForResult(roles.createRequestRoleIntent(RoleManager.ROLE_DIALER), REQUEST_ROLE)
            } else {
                notice("Secretary already handles phone calls.")
                notifications()
            }
        }
        text("Phone number", 14)
        number = EditText(this)
        number.setTextColor(-1)
        number.inputType = InputType.TYPE_CLASS_PHONE
        body.addView(number)
        intent.data?.let { number.setText(it.schemeSpecificPart) }
        button("Call this number") { dial() }
        button("Call history & transcripts") { startActivity(Intent(this, TranscriptActivity::class.java)) }
        text(
            "SIM calls use Android’s phone role. Recording both sides is not available to a normal app. " +
                "Import an existing call recording to transcribe it. Internet conversations here are with Lucina; " +
                "other VoIP services are not connected.",
            13,
        )
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
            notice("Call permission enabled. Tap Call this number when ready.")
        }
    }

    private companion object {
        const val REQUEST_ROLE = 1
        const val REQUEST_CALL = 2
        const val REQUEST_NOTIFICATIONS = 4
    }
}
