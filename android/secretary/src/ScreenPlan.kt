package tech.getsecretary.secretary

/**
 * From Rust's decision to Telecom's `CallResponse`, with no Android in it (host-tested by test/run.sh).
 * The policy itself is the shared core's `reject_decision`, run in Rust; this only refuses to act on it unless
 * every check ran and the verdict was in time.
 */
object ScreenPlan {
    /** The flags of `CallScreeningService.CallResponse.Builder`. */
    data class Response(val disallow: Boolean, val reject: Boolean, val skipLog: Boolean, val skipNotification: Boolean)

    /**
     * `reject`: disallow and reject, keep the call log entry, skip Telecom's missed-call notification (the
     * app posts its own quiet one with the Allow buttons). Anything else puts the call through unchanged.
     */
    fun response(action: String): Response =
        if (action == "reject") Response(disallow = true, reject = true, skipLog = false, skipNotification = true)
        else Response(disallow = false, reject = false, skipLog = false, skipNotification = false)

    /** What the phone could check at ring time. A check that did not run is `false` in its `...Ran`. */
    data class Checks(
        val emergency: Boolean,
        val contactRan: Boolean,
        val contactName: String?,
        val callsRan: Boolean,
        val calledOrAnswered: Boolean,
    )

    /** Some check did not run (permission missing, query failed): the call is never rejected. */
    fun unchecked(c: Checks): Boolean = !c.contactRan || !c.callsRan

    /**
     * The action to take: a rejection only if Rust said `reject`, the screening waited for it, every guard was
     * checked and the verdict came inside the limit. Everything else is `allow`.
     */
    fun action(rust: String?, waited: Boolean, unchecked: Boolean, inTime: Boolean): String =
        if (rust == "reject" && waited && !unchecked && inTime) "reject" else "allow"
}
