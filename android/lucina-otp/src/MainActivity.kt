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
import android.view.View
import android.widget.Switch
import android.widget.TextView
import org.json.JSONObject
import tech.getsecretary.design.LucinaUi
import tech.getsecretary.design.ValueRow
import java.util.concurrent.Executors

class MainActivity : Activity() {
    private lateinit var ui: LucinaUi
    private lateinit var summary: TextView
    private lateinit var error: TextView
    private lateinit var sms: ValueRow
    private lateinit var forwarding: ValueRow
    private lateinit var pairing: ValueRow
    private lateinit var notifRow: ValueRow
    private lateinit var pending: ValueRow
    private lateinit var last: ValueRow
    private lateinit var parser: ValueRow
    private lateinit var version: ValueRow
    private lateinit var access: ValueRow
    private lateinit var notify: Switch
    private lateinit var test: ValueRow
    private val worker = Executors.newSingleThreadExecutor()

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        cleanupLegacyNotification(this)
        ui = LucinaUi(this)
        val page = ui.screen(this)
        page.addView(ui.largeTitle("Lucina OTP"))
        summary = ui.chip("Checking…", ui.c.textSecondary)
        page.addView(summary)
        error = ui.footnote("").apply { setTextColor(ui.c.negative); visibility = View.GONE }
        page.addView(error)

        page.addView(ui.sectionHeader("Status"))
        sms = ui.valueRow("Incoming SMS")
        forwarding = ui.valueRow("Forwarding")
        pairing = ui.valueRow("Internet pairing")
        notifRow = ui.valueRow("Notifications")
        pending = ui.valueRow("Queued")
        last = ui.valueRow("Last result")
        parser = ui.valueRow("Parser self-test")
        version = ui.valueRow("Version")
        page.addView(ui.group(sms.view, forwarding.view, pairing.view, notifRow.view, pending.view, last.view, parser.view, version.view))

        page.addView(ui.sectionHeader("SMS forwarding"))
        page.addView(ui.card().apply {
            addView(ui.pair(
                ui.primaryButton("Enable") {
                    if (checkSelfPermission(Manifest.permission.RECEIVE_SMS) != PackageManager.PERMISSION_GRANTED) {
                        requestPermissions(arrayOf(Manifest.permission.RECEIVE_SMS), 1)
                    } else {
                        configure(JSONObject().put("enabled", true))
                    }
                },
                ui.secondaryButton("Disable") { configure(JSONObject().put("enabled", false)) },
            ))
        })

        page.addView(ui.sectionHeader("Notifications and relay"))
        access = ui.valueRow("Notification access", chevron = true) {
            startActivity(Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS))
        }
        val (toggle, sw) = ui.toggleRow("Forward notifications") { on -> configure(JSONObject().put("notifications", on)) }
        notify = sw
        test = ui.actionRow("Test connection") {
            test.set("Testing…")
            background {
                val r = JSONObject(Native.selfTest(true))
                val result = r.optString("result")
                val parse = r.optBoolean("parse")
                val line = "${label(result)} in ${r.optLong("ms")} ms" + if (parse) "" else ", parser FAILED"
                runOnUiThread { test.set(line, if (parse) resultColor(result) else ui.c.negative) }
                refresh()
            }
        }
        page.addView(ui.group(access.view, toggle, test.view))

        page.addView(ui.footnote(
            "Forwards one-time codes from incoming SMS (and, if you allow it, notifications) to your private " +
                "Lucina chat. Android wakes the app for each SMS; it sends over HTTPS and exits. No running service, " +
                "SMS history, contacts or sending permission. Codes expire within minutes; match the code to the " +
                "service you requested.",
        ).apply { (layoutParams as android.widget.LinearLayout.LayoutParams).topMargin = ui.dp(20f) })
    }

    override fun onResume() {
        super.onResume()
        background { refresh() }
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
            background { refresh() }
        }
    }

    private fun configure(patch: JSONObject) = background {
        Native.configure(patch.toString())
        refresh()
    }

    /** Runs native work off the main thread; any failure shows up under the title. */
    private fun background(work: () -> Unit) {
        try {
            worker.execute {
                try {
                    Core.ensure(this)
                    work()
                } catch (t: Throwable) {
                    runOnUiThread {
                        error.text = "Core unavailable: ${t.javaClass.simpleName}"
                        error.visibility = View.VISIBLE
                    }
                }
            }
        } catch (_: java.util.concurrent.RejectedExecutionException) {
        }
    }

    private fun label(result: String) = result.ifEmpty { "none" }.replace('_', ' ')

    private fun resultColor(result: String): Int? = when (result) {
        "sent" -> ui.c.positive
        "queued", "paused", "duplicate" -> ui.c.warning
        "", "disabled", "not_otp" -> null
        else -> ui.c.negative
    }

    /** Worker thread: reads status() and posts it to the rows. */
    private fun refresh() {
        val s = JSONObject(Native.status())
        val smsOk = checkSelfPermission(Manifest.permission.RECEIVE_SMS) == PackageManager.PERMISSION_GRANTED
        val granted = Settings.Secure.getString(contentResolver, "enabled_notification_listeners")
            ?.contains(packageName) == true
        val on = s.optBoolean("notifications")
        val enabled = s.optBoolean("enabled")
        val paired = s.optBoolean("paired")
        val queued = s.optInt("pending")
        val result = if (s.isNull("lastResult")) "" else s.optString("lastResult")
        val latency = if (s.has("lastLatencyMs") && !s.isNull("lastLatencyMs")) " · ${s.optLong("lastLatencyMs")} ms" else ""
        val selfTest = s.optBoolean("selfTest")
        val ver = "${s.optString("version")} · protocol ${s.optInt("protocol")}"
        runOnUiThread {
            val c = ui.c
            error.visibility = View.GONE
            sms.set(if (smsOk) "Allowed" else "Not allowed", if (smsOk) c.positive else c.negative)
            forwarding.set(if (enabled) "Enabled" else "Off", if (enabled) c.positive else c.warning)
            pairing.set(if (paired) "Ready" else "Not configured", if (paired) c.positive else c.negative)
            notifRow.set(
                (if (on) "Forwarded" else "Off") + if (granted) ", access granted" else ", no access",
                if (on && granted) c.positive else if (on) c.warning else null,
            )
            pending.set(queued.toString(), if (queued > 0) c.warning else null)
            last.set(label(result) + latency, resultColor(result))
            parser.set(if (selfTest) "OK" else "FAILED", if (selfTest) c.positive else c.negative)
            version.set(ver)
            access.set(if (granted) "Granted" else "Not granted")
            notify.isChecked = on
            when {
                !smsOk || !paired || !selfTest -> ui.recolor(summary, "Needs setup", c.negative)
                !enabled -> ui.recolor(summary, "Forwarding off", c.warning)
                queued > 0 -> ui.recolor(summary, "$queued queued", c.warning)
                else -> ui.recolor(summary, "Forwarding active", c.positive)
            }
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
