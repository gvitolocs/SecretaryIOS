package tech.getsecretary.secretary

/**
 * Where a dialed string goes, with no Android and no JNI in it, so it is unit-tested on the host
 * (`android/secretary/test/run.sh`) and cannot be broken by the Rust core or the network.
 *
 * An emergency number is decided first, before any other check, and goes straight to Telecom's
 * `placeCall`: no permission gating of ours, no confirmation, no Jev, no journal. Everything else must be
 * a plain number (3 to 15 digits, one leading `+`), like Rust's `number::call_number` (USSD and service
 * codes are not voice calls), optionally followed by post-dial digits: a `,` pause or `;` wait and then
 * digits, `*` or `#` (masked numbers from delivery apps carry their extension this way, e.g.
 * `+4512345678,1234`). Android's `p`/`w` spellings become `,`/`;`. Telecom dials the post-dial part itself.
 */
object DialPlan {
    enum class Kind { EMERGENCY, CALL, INVALID }

    data class Plan(val kind: Kind, val number: String)

    /** The placing of a call: Telecom's `placeCall(Uri.fromParts(scheme, number, null), Bundle())`. */
    fun interface Placer {
        fun placeCall(scheme: String, number: String)
    }

    sealed class Outcome {
        data class Placed(val number: String, val emergency: Boolean) : Outcome()
        object Invalid : Outcome()
        /** Telecom refused (for example no CALL_PHONE). An emergency number then falls back to the system dialer. */
        data class Failed(val number: String, val emergency: Boolean) : Outcome()
    }

    /** Spaces, dots, dashes and brackets removed. */
    fun normalize(raw: String): String = raw.filter { !it.isWhitespace() && it != '(' && it != ')' && it != '-' && it != '.' }

    private fun plain(n: String): Boolean {
        val digits = if (n.startsWith("+")) n.substring(1) else n
        return digits.length in 3..15 && digits.all { it in '0'..'9' }
    }

    /** Characters of a post-dial part (after the first `,` or `;`). */
    private const val MAX_POST_DIAL = 32

    /** `+4512345678,1234#` → (`+4512345678`, `,1234#`); `null` when the part after the pause is not dialable. */
    private fun split(n: String): Pair<String, String>? {
        val i = n.indexOfFirst { it == ',' || it == ';' }
        if (i < 0) return n to ""
        val post = n.substring(i)
        val ok = post.length in 2..MAX_POST_DIAL && post.all { it in '0'..'9' || it in ",;*#" }
        return if (ok) n.substring(0, i) to post else null
    }

    /** A dialed string's route. [isEmergency] is `TelephonyManager.isEmergencyNumber`; a throwing check counts as "no". */
    fun plan(raw: String, isEmergency: (String) -> Boolean): Plan {
        val n = normalize(raw).map { c ->
            when (c) {
                'p', 'P' -> ','
                'w', 'W' -> ';'
                else -> c
            }
        }.joinToString("")
        if (n.isEmpty() || n.length > 64) return Plan(Kind.INVALID, "")
        val parts = split(n)
        val core = parts?.first ?: n
        val emergency = try {
            isEmergency(n) || (core != n && isEmergency(core))
        } catch (t: Throwable) {
            false
        }
        return when {
            // An emergency call never carries post-dial digits of ours.
            emergency -> Plan(Kind.EMERGENCY, core)
            parts != null && plain(core) -> Plan(Kind.CALL, core + parts.second)
            else -> Plan(Kind.INVALID, "")
        }
    }

    /** Plans and places [raw]. Never throws. */
    fun dial(raw: String, isEmergency: (String) -> Boolean, placer: Placer): Outcome {
        val p = plan(raw, isEmergency)
        if (p.kind == Kind.INVALID) return Outcome.Invalid
        val emergency = p.kind == Kind.EMERGENCY
        return try {
            placer.placeCall("tel", p.number)
            Outcome.Placed(p.number, emergency)
        } catch (t: Throwable) {
            Outcome.Failed(p.number, emergency)
        }
    }

    /** The carrier voicemail: `voicemail:` through Telecom. */
    fun voicemail(placer: Placer): Boolean = try {
        placer.placeCall("voicemail", "")
        true
    } catch (t: Throwable) {
        false
    }
}
