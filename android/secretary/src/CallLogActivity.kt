package tech.getsecretary.secretary

import android.Manifest
import android.app.NotificationManager
import android.content.ContentValues
import android.content.pm.PackageManager
import android.os.Bundle
import android.provider.CallLog
import android.telecom.TelecomManager
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import java.util.Locale
import java.util.TimeZone
import org.json.JSONArray
import org.json.JSONObject
import tech.getsecretary.design.LucinaTokens.TextSize
import tech.getsecretary.design.LucinaTokens.TextWeight

/**
 * Recent calls from the system call log (READ_CALL_LOG, which the phone role grants). Rust formats the rows and
 * adds Jev's remembered verdict for numbers that are not contacts. A tap calls back (through Telecom).
 */
class CallLogActivity : Ui() {
    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        screen("Recent calls")
    }

    override fun onResume() {
        super.onResume()
        load()
    }

    private fun clear() {
        while (body.childCount > 1) body.removeViewAt(1)
    }

    private fun load() {
        clear()
        if (checkSelfPermission(Manifest.permission.READ_CALL_LOG) != PackageManager.PERMISSION_GRANTED) {
            body.addView(ui.footnote("Allow call log access to see your recent calls here. Making Secretary your phone app grants it."))
            body.addView(ui.primaryButton("Allow call log") {
                requestPermissions(arrayOf(Manifest.permission.READ_CALL_LOG, Manifest.permission.WRITE_CALL_LOG), 30)
            }.apply { layoutParams = ui.lp(top = 16f) })
            return
        }
        val app = applicationContext
        val dir = filesDir.absolutePath
        Calls.io.execute {
            val rows = safely<JSONArray?>(null) { read(app) }
            val shown = safely<JSONArray?>(null) {
                val req = JSONObject().put("rows", rows)
                    .put("tzOffsetMin", TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 60000)
                    .put("lang", Locale.getDefault().language)
                unwrap(Native.dialer(dir, "callLog", req.toString())).getJSONArray("rows")
            }
            runOnUiThread { if (!isDestroyed) show(shown, rows) }
        }
        cancelMissed()
    }

    /** The newest 200 rows, in the shape Rust's `callLog` wants. */
    private fun read(c: android.content.Context): JSONArray {
        val out = JSONArray()
        val q = android.os.Bundle().apply {
            putInt(android.content.ContentResolver.QUERY_ARG_LIMIT, 200)
            putStringArray(android.content.ContentResolver.QUERY_ARG_SORT_COLUMNS, arrayOf(CallLog.Calls.DATE))
            putInt(android.content.ContentResolver.QUERY_ARG_SORT_DIRECTION, android.content.ContentResolver.QUERY_SORT_DIRECTION_DESCENDING)
        }
        c.contentResolver.query(
            CallLog.Calls.CONTENT_URI,
            arrayOf(CallLog.Calls._ID, CallLog.Calls.NUMBER, CallLog.Calls.TYPE, CallLog.Calls.DATE, CallLog.Calls.DURATION, CallLog.Calls.CACHED_NAME, CallLog.Calls.NEW),
            q, null,
        )?.use {
            while (it.moveToNext()) {
                out.put(
                    JSONObject().put("id", it.getLong(0)).put("number", it.getString(1) ?: "").put("type", it.getInt(2))
                        .put("dateMs", it.getLong(3)).put("durationS", it.getLong(4))
                        .put("name", it.getString(5) ?: JSONObject.NULL).put("isNew", it.getInt(6) != 0),
                )
            }
        }
        return out
    }

    private fun show(shown: JSONArray?, raw: JSONArray?) {
        clear()
        if (shown == null || shown.length() == 0) {
            body.addView(ui.footnote(if (raw == null) "The call log is unavailable." else "No calls yet."))
            return
        }
        val rows = ArrayList<View>()
        for (i in 0 until shown.length()) {
            val r = shown.getJSONObject(i)
            val number = r.getString("number")
            val column = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                layoutParams = ui.lp(w = 0, weight = 1f)
                addView(ui.text(r.getString("title"), TextSize.body, if (r.optBoolean("isNew")) TextWeight.headline else TextWeight.body,
                    if (r.optBoolean("missed")) ui.c.negative else ui.c.text))
                addView(ui.text(r.getString("subtitle"), TextSize.footnote, TextWeight.footnote, ui.c.textSecondary))
                if (r.has("jev") && !r.isNull("jev")) {
                    addView(ui.text(r.getString("jev"), TextSize.footnote, TextWeight.footnote, if (r.optBoolean("warn")) ui.c.negative else ui.c.accent))
                }
            }
            rows.add(ui.row().apply {
                gravity = Gravity.CENTER_VERTICAL
                addView(column)
                if (number.isNotEmpty()) {
                    isClickable = true
                    setOnClickListener { callBack(number) }
                }
            })
        }
        body.addView(ui.group(*rows.toTypedArray()).apply { layoutParams = ui.lp(top = 12f) })
        body.addView(ui.footnote("Tap a row to call back."))
    }

    private fun callBack(number: String) {
        if (checkSelfPermission(Manifest.permission.CALL_PHONE) != PackageManager.PERMISSION_GRANTED &&
            !Phone.isEmergency(this, DialPlan.normalize(number))
        ) {
            requestPermissions(arrayOf(Manifest.permission.CALL_PHONE), 31)
            return
        }
        if (Phone.dial(this, number) is DialPlan.Outcome.Invalid) notice("That number cannot be called.")
    }

    /** Viewing the log clears the missed-call notification and marks the missed calls as seen. */
    private fun cancelMissed() {
        safely(Unit) { getSystemService(NotificationManager::class.java).cancel(PhoneEventsReceiver.MISSED_NOTIFICATION) }
        safely(Unit) { if (Phone.isDefaultDialer(this)) getSystemService(TelecomManager::class.java).cancelMissedCallsNotification() }
        if (checkSelfPermission(Manifest.permission.WRITE_CALL_LOG) == PackageManager.PERMISSION_GRANTED) {
            val app = applicationContext
            Calls.io.execute {
                safely(Unit) {
                    app.contentResolver.update(
                        CallLog.Calls.CONTENT_URI,
                        ContentValues().apply { put(CallLog.Calls.NEW, 0); put(CallLog.Calls.IS_READ, 1) },
                        "${CallLog.Calls.TYPE} = ? AND (${CallLog.Calls.NEW} = 1 OR ${CallLog.Calls.IS_READ} = 0)",
                        arrayOf(CallLog.Calls.MISSED_TYPE.toString()),
                    )
                }
            }
        }
    }

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        if (r == 30) load()
    }
}
