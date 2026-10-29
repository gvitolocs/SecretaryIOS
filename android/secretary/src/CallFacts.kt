package tech.getsecretary.secretary

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Bundle
import android.provider.CallLog
import org.json.JSONObject

/**
 * What only this phone knows about a ringing number: is it a contact, did he call it or answer it before, is it
 * an emergency number. A check that cannot run (permission missing, query failed) is reported as not run, and
 * then nothing is rejected ([ScreenPlan.unchecked]). The policy is Rust's; this only gathers facts.
 */
object CallFacts {
    fun collect(c: Context, number: String): ScreenPlan.Checks {
        val emergency = safely(true) { Phone.isEmergency(c, DialPlan.normalize(number)) }
        var contactRan = false
        var contact: String? = null
        if (number.isNotEmpty() && c.checkSelfPermission(Manifest.permission.READ_CONTACTS) == PackageManager.PERMISSION_GRANTED) {
            safely(Unit) {
                contact = LucinaCallService.contactName(c, number)
                contactRan = true
            }
        }
        var callsRan = false
        var called = false
        if (number.isNotEmpty() && c.checkSelfPermission(Manifest.permission.READ_CALL_LOG) == PackageManager.PERMISSION_GRANTED) {
            safely(Unit) {
                called = calledOrAnswered(c, number)
                callsRan = true
            }
        }
        // A hidden number has nothing to look up: nothing to protect either, and Rust never rejects it.
        if (number.isEmpty()) {
            contactRan = true
            callsRan = true
        }
        return ScreenPlan.Checks(emergency, contactRan, contact, callsRan, called)
    }

    /** An outgoing call to the number, or an incoming one he answered (duration > 0), in the call log. Throws if it cannot ask. */
    private fun calledOrAnswered(c: Context, number: String): Boolean {
        val digits = number.filter { it in '0'..'9' }
        if (digits.isEmpty()) return false
        val tail = digits.takeLast(8)
        val q = Bundle().apply {
            putString(
                android.content.ContentResolver.QUERY_ARG_SQL_SELECTION,
                "(${CallLog.Calls.TYPE} = ? OR (${CallLog.Calls.TYPE} = ? AND ${CallLog.Calls.DURATION} > 0)) AND ${CallLog.Calls.NUMBER} LIKE ?",
            )
            putStringArray(
                android.content.ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS,
                arrayOf(CallLog.Calls.OUTGOING_TYPE.toString(), CallLog.Calls.INCOMING_TYPE.toString(), "%$tail"),
            )
            putInt(android.content.ContentResolver.QUERY_ARG_LIMIT, 1)
        }
        val cursor = c.contentResolver.query(CallLog.Calls.CONTENT_URI, arrayOf(CallLog.Calls._ID), q, null)
            ?: throw IllegalStateException("call log unavailable")
        return cursor.use { it.moveToFirst() }
    }

    /** The input Rust's `screenCall` and `plan` read. */
    fun json(k: ScreenPlan.Checks): JSONObject {
        val o = JSONObject()
        k.contactName?.let { o.put("contact", it) }
        return o.put("calledOrAnswered", k.calledOrAnswered).put("emergency", k.emergency).put("unchecked", ScreenPlan.unchecked(k))
    }
}
