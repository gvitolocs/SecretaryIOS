package tech.getsecretary.secretary

import android.content.ContentProvider
import android.content.ContentValues
import android.database.Cursor
import android.net.Uri
import android.os.Binder
import android.os.Bundle
import android.os.Process

/**
 * Lucina's phone MCP over authenticated adb:
 * `content call --uri content://tech.getsecretary.secretary.calls --method call|check --arg <number>`.
 * Returns `{"result": <Native.phoneCall JSON>}`; the same Rust function as the Call button.
 * `--method emergency --arg 112` only reports how a string would be routed (never dials; no Rust).
 */
class CallProvider : ContentProvider() {
    override fun onCreate(): Boolean = true

    override fun call(method: String, arg: String?, extras: Bundle?): Bundle {
        val uid = Binder.getCallingUid()
        if (uid != 2000 && uid != 0 && uid != Process.myUid()) throw SecurityException("Owner-authorized device bridge only")
        val context = context ?: throw IllegalStateException("Provider not attached")
        val b = Bundle()
        if (method == "emergency") {
            // Dry run for the device checklist: how a dialed string would be routed. Nothing is dialed, ever.
            b.putString("result", routeOf(context, arg ?: ""))
            return b
        }
        if (method != "call" && method != "check") throw IllegalArgumentException("Unknown phone function")
        b.putString("result", Native.phoneCall(context, arg ?: "", method == "check"))
        return b
    }

    /** `{"number", "kind", "systemSays", "route"}` for [raw]: the [DialPlan] route and what Telephony says; no Rust, no call. */
    private fun routeOf(c: android.content.Context, raw: String): String {
        val plan = DialPlan.plan(raw) { Phone.isEmergency(c, it) }
        val system = try {
            c.getSystemService(android.telephony.TelephonyManager::class.java).isEmergencyNumber(plan.number).toString()
        } catch (t: Throwable) {
            "unavailable: " + t.javaClass.simpleName
        }
        val route = when (plan.kind) {
            DialPlan.Kind.EMERGENCY -> "TelecomManager.placeCall(tel:" + plan.number + ") at once"
            DialPlan.Kind.CALL -> "TelecomManager.placeCall(tel:" + plan.number + ") after the CALL_PHONE check"
            DialPlan.Kind.INVALID -> "refused"
        }
        return org.json.JSONObject().put("number", plan.number).put("kind", plan.kind.name).put("systemSays", system).put("route", route)
            .put("defaultDialer", Phone.isDefaultDialer(c)).put("placed", false).toString()
    }

    override fun query(u: Uri, p: Array<String>?, s: String?, a: Array<String>?, o: String?): Cursor = throw SecurityException()

    override fun getType(u: Uri): String? = null

    override fun insert(u: Uri, v: ContentValues?): Uri = throw SecurityException()

    override fun delete(u: Uri, s: String?, a: Array<String>?): Int = throw SecurityException()

    override fun update(u: Uri, v: ContentValues?, s: String?, a: Array<String>?): Int = throw SecurityException()
}
