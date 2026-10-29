import tech.getsecretary.secretary.CallPlan
import tech.getsecretary.secretary.DialPlan
import tech.getsecretary.secretary.DialPlan.Kind
import tech.getsecretary.secretary.DialPlan.Outcome
import tech.getsecretary.secretary.ScreenPlan

/** Host tests for the pure Kotlin logic (run by android/secretary/test/run.sh). Exit code 1 on any failure. */
var failures = 0
var checks = 0

fun check(ok: Boolean, what: String) {
    checks++
    if (!ok) {
        failures++
        println("FAIL: $what")
    }
}

fun <T> same(actual: T, expected: T, what: String) = check(actual == expected, "$what: expected $expected, got $actual")

/** A fake TelephonyManager.isEmergencyNumber for Denmark and a few other countries. */
val emergency = setOf("112", "999", "911", "000", "110", "118")
fun isEmergency(n: String) = n in emergency

class Recorder(private val fail: Boolean = false) : DialPlan.Placer {
    val placed = ArrayList<Pair<String, String>>()
    override fun placeCall(scheme: String, number: String) {
        if (fail) throw SecurityException("no CALL_PHONE")
        placed.add(scheme to number)
    }
}

fun main() {
    // Emergency numbers go through Telecom's placeCall as tel:, whatever their shape, spaces or length.
    for (n in listOf("112", " 1 1 2 ", "999", "911", "000", "110", "118")) {
        val r = Recorder()
        val out = DialPlan.dial(n, ::isEmergency, r)
        same(out, Outcome.Placed(DialPlan.normalize(n), true), "emergency $n")
        same(r.placed, listOf("tel" to DialPlan.normalize(n)), "emergency $n goes to placeCall")
    }
    // The emergency check never depends on anything else: a throwing detector means "not an emergency", a throwing placer is reported.
    same(DialPlan.plan("112") { throw IllegalStateException("telephony down") }.kind, Kind.CALL, "detector failure")
    val refused = Recorder(fail = true)
    same(DialPlan.dial("112", ::isEmergency, refused), Outcome.Failed("112", true), "refused emergency is reported, not thrown")
    // Emergency numbers are never "invalid", even when shorter than a normal number.
    same(DialPlan.plan("112", ::isEmergency).kind, Kind.EMERGENCY, "112 plan")
    same(DialPlan.plan("12", ::isEmergency).kind, Kind.INVALID, "12 is not a number")
    // Normal numbers.
    same(DialPlan.plan("+45 66 77 87 02", ::isEmergency), DialPlan.Plan(Kind.CALL, "+4566778702"), "danish number")
    same(DialPlan.plan("(+45) 66-77-87-02", ::isEmergency), DialPlan.Plan(Kind.CALL, "+4566778702"), "brackets")
    same(DialPlan.plan("66778702", ::isEmergency).kind, Kind.CALL, "national number")
    same(DialPlan.plan("12345", ::isEmergency).kind, Kind.CALL, "five digits are a plain number here, Telecom decides")
    for (bad in listOf("", "  ", "*21*123#", "*#06#", "+45;reboot", "++4512345678", "4512,12", "1".repeat(16), "+", "tel:+4512345678", "abc")) {
        same(DialPlan.plan(bad, ::isEmergency).kind, Kind.INVALID, "invalid '$bad'")
    }
    val r = Recorder()
    same(DialPlan.dial("*21#", ::isEmergency, r), Outcome.Invalid, "ussd is not dialed")
    check(r.placed.isEmpty(), "nothing placed for an invalid string")
    same(DialPlan.dial("+4566778702", ::isEmergency, r), Outcome.Placed("+4566778702", false), "normal call")
    same(r.placed, listOf("tel" to "+4566778702"), "normal call goes to placeCall")
    // Voicemail.
    val v = Recorder()
    check(DialPlan.voicemail(v), "voicemail placed")
    same(v.placed, listOf("voicemail" to ""), "voicemail scheme")
    check(!DialPlan.voicemail(Recorder(fail = true)), "voicemail refusal is reported")

    // The fallback call view (used when Rust cannot answer) has what an emergency call needs.
    val calls = listOf(CallPlan.CallState(CallPlan.DIALING, "112", false))
    val v1 = CallPlan.fallback(calls, false, CallPlan.EARPIECE, 0)
    same(v1.rows[0].peer, "112", "emergency peer")
    same(v1.rows[0].label, "Dialing", "emergency label")
    same(v1.rows[0].actions, listOf("end"), "emergency can be ended")
    check(v1.notice!!.fullScreen, "emergency dialing opens the screen")
    same(v1.notice!!.actions, listOf("hangup"), "emergency notification action")
    same(v1.controls.routes.map { it.name }, listOf("Earpiece", "Speaker"), "routes before the phone reports")
    check(v1.controls.proximity, "emergency at the ear turns the screen off near the face")
    // Same table as Rust's ui::call_view_with.
    val waiting = CallPlan.fallback(listOf(CallPlan.CallState(CallPlan.ACTIVE, "+4511111111", true), CallPlan.CallState(CallPlan.RINGING, null, false)), true, CallPlan.SPEAKER, CallPlan.EARPIECE or CallPlan.BLUETOOTH or CallPlan.SPEAKER)
    same(waiting.rows[0].actions, listOf("end", "hold"), "active")
    same(waiting.rows[1].actions, listOf("answer", "end_answer", "decline"), "call waiting")
    same(waiting.rows[1].peer, "Private number", "private")
    same(waiting.notice!!.index, 1, "notification is about the ringing call")
    same(waiting.notice!!.actions, listOf("answer", "decline"), "answer and decline only on tap")
    same(waiting.controls.routes.map { it.name to it.on }, listOf("Earpiece" to false, "Bluetooth" to false, "Speaker" to true), "routes")
    check(!waiting.controls.proximity && waiting.controls.muted, "speaker: no proximity lock; muted flag kept")
    same(CallPlan.fallback(listOf(CallPlan.CallState(CallPlan.HOLDING, "+45", true)), false, 1, 1).rows[0].actions, listOf("end", "resume"), "holding")
    same(CallPlan.fallback(emptyList(), false, 1, 1).notice, null, "no calls, no notification")
    // safely() swallows Errors such as a missing native library.
    same(tech.getsecretary.secretary.safely("x") { throw UnsatisfiedLinkError("no lib") }, "x", "safely")

    // Decision -> CallResponse.
    same(ScreenPlan.response("reject"), ScreenPlan.Response(disallow = true, reject = true, skipLog = false, skipNotification = true), "reject response")
    val allow = ScreenPlan.Response(disallow = false, reject = false, skipLog = false, skipNotification = false)
    for (a in listOf("allow", "silence", "", "REJECT", "rejected", "block")) same(ScreenPlan.response(a), allow, "'$a' puts the call through")
    // Never reject when any guard is unknown, the screening did not wait, or the verdict was late.
    same(ScreenPlan.action("reject", waited = true, unchecked = false, inTime = true), "reject", "all clear")
    same(ScreenPlan.action("reject", waited = true, unchecked = true, inTime = true), "allow", "a guard could not be checked")
    same(ScreenPlan.action("reject", waited = false, unchecked = false, inTime = true), "allow", "guards said no wait")
    same(ScreenPlan.action("reject", waited = true, unchecked = false, inTime = false), "allow", "late verdict")
    same(ScreenPlan.action("allow", waited = true, unchecked = false, inTime = true), "allow", "rust said allow")
    same(ScreenPlan.action(null, waited = true, unchecked = false, inTime = true), "allow", "no answer")
    same(ScreenPlan.action("silence", waited = true, unchecked = false, inTime = true), "allow", "no silence mode")
    val ok = ScreenPlan.Checks(emergency = false, contactRan = true, contactName = null, callsRan = true, calledOrAnswered = false)
    check(!ScreenPlan.unchecked(ok), "all checks ran")
    check(ScreenPlan.unchecked(ok.copy(contactRan = false)), "contacts not checked")
    check(ScreenPlan.unchecked(ok.copy(callsRan = false)), "call log not checked")
    check(ScreenPlan.unchecked(ok.copy(contactRan = false, callsRan = false)), "neither checked")
    // Every combination of unknowns: unchecked => allow, whatever Rust said.
    for (c in listOf(true, false)) for (l in listOf(true, false)) {
        val u = ScreenPlan.unchecked(ok.copy(contactRan = c, callsRan = l))
        same(ScreenPlan.action("reject", true, u, true), if (c && l) "reject" else "allow", "contacts=$c calls=$l")
    }

    println("$checks checks, $failures failures")
    if (failures > 0) System.exit(1)
}
