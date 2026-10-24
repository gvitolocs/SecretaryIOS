package tech.getsecretary.lucinaotp

import android.Manifest
import android.app.Activity
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.provider.Settings
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONObject
import java.util.concurrent.Executors

class MainActivity : Activity() {
    private lateinit var status: TextView
    private lateinit var notify: Button
    private val worker = Executors.newSingleThreadExecutor()
    private var notifications = false

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        cleanupLegacyNotification(this)
        val panel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(32, 48, 32, 32)
        }
        setContentView(ScrollView(this).apply { addView(panel) })
        panel.addView(TextView(this).apply { text = "Lucina OTP"; textSize = 28f })
        panel.addView(TextView(this).apply {
            text = "Forwards one-time codes from incoming SMS (and, if you allow it, notifications) to your private " +
                "Lucina chat. Android wakes the app for each SMS; it sends over HTTPS and exits. No running service, " +
                "SMS history, contacts or sending permission. Codes expire within minutes; match the code to the " +
                "service you requested.\n"
        })
        status = TextView(this)
        panel.addView(status)
        panel.addView(button("Enable SMS forwarding") {
            if (checkSelfPermission(Manifest.permission.RECEIVE_SMS) != PackageManager.PERMISSION_GRANTED) {
                requestPermissions(arrayOf(Manifest.permission.RECEIVE_SMS), 1)
            } else {
                configure(JSONObject().put("enabled", true))
            }
        })
        panel.addView(button("Disable forwarding") { configure(JSONObject().put("enabled", false)) })
        panel.addView(button("Notification access") {
            startActivity(Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS))
        })
        notify = button("Forward notifications: …") { configure(JSONObject().put("notifications", !notifications)) }
        panel.addView(notify)
        panel.addView(button("Test connection") {
            status.text = "Testing…\n"
            background {
                val r = JSONObject(Native.selfTest(true))
                val line = "Test: ${r.optString("result")} in ${r.optLong("ms")} ms (parser ${if (r.optBoolean("parse")) "ok" else "FAILED"})"
                refresh(line)
            }
        })
    }

    override fun onResume() {
        super.onResume()
        background { refresh(null) }
    }

    override fun onDestroy() {
        worker.shutdown()
        super.onDestroy()
    }

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        if (r == 1 && checkSelfPermission(Manifest.permission.RECEIVE_SMS) == PackageManager.PERMISSION_GRANTED) {
            configure(JSONObject().put("enabled", true))
        } else {
            background { refresh(null) }
        }
    }

    private fun button(label: String, onClick: () -> Unit) = Button(this).apply {
        text = label
        setOnClickListener { onClick() }
    }

    private fun configure(patch: JSONObject) = background {
        Native.configure(patch.toString())
        refresh(null)
    }

    /** Runs native work off the main thread; any failure shows up in the status line. */
    private fun background(work: () -> Unit) {
        try {
            worker.execute {
                try {
                    Core.ensure(this)
                    work()
                } catch (t: Throwable) {
                    runOnUiThread { status.text = "Core unavailable: ${t.javaClass.simpleName}\n" }
                }
            }
        } catch (_: java.util.concurrent.RejectedExecutionException) {
        }
    }

    /** Worker thread: reads status() and posts the text to the UI. */
    private fun refresh(extra: String?) {
        val s = JSONObject(Native.status())
        val sms = checkSelfPermission(Manifest.permission.RECEIVE_SMS) == PackageManager.PERMISSION_GRANTED
        val access = Settings.Secure.getString(contentResolver, "enabled_notification_listeners")
            ?.contains(packageName) == true
        val on = s.optBoolean("notifications")
        val last = s.optString("lastResult").ifEmpty { "none" }
        val latency = if (s.has("lastLatencyMs") && !s.isNull("lastLatencyMs")) " (${s.optLong("lastLatencyMs")} ms)" else ""
        val text = buildString {
            append("Incoming SMS: ").append(if (sms) "allowed" else "not allowed").append('\n')
            append("Forwarding: ").append(if (s.optBoolean("enabled")) "enabled" else "off").append('\n')
            append("Notifications: ").append(if (on) "forwarded" else "off")
                .append(if (access) ", access granted" else ", no access").append('\n')
            append("Internet pairing: ").append(if (s.optBoolean("paired")) "ready" else "not configured").append('\n')
            append("Queued: ").append(s.optInt("pending")).append('\n')
            append("Last: ").append(last).append(latency).append('\n')
            append("Parser self-test: ").append(if (s.optBoolean("selfTest")) "ok" else "FAILED").append('\n')
            append("Version ").append(s.optString("version")).append(", protocol ").append(s.optInt("protocol")).append('\n')
            if (extra != null) append(extra).append('\n')
        }
        runOnUiThread {
            notifications = on
            notify.text = if (on) "Stop forwarding notifications" else "Forward notifications"
            status.text = text
        }
    }

    companion object {
        /** 0.4.0 removed the old foreground service; clear its leftover notification and channel. */
        @JvmStatic
        fun cleanupLegacyNotification(c: Context) {
            val nm = c.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager? ?: return
            nm.cancelAll()
            if (Build.VERSION.SDK_INT >= 26) nm.deleteNotificationChannel("otp")
        }
    }
}
