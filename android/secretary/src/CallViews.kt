package tech.getsecretary.secretary

import org.json.JSONArray
import org.json.JSONObject

/** Rust's `ui` `callView` answer as the plain models [CallPlan] also produces. */
object CallViews {
    /** One live call as the in-call service knows it. */
    class Live(
        val state: Int,
        val handle: String?,
        val canHold: Boolean,
        val name: String?,
        val jev: String?,
        val jevSpam: Double,
    )

    class Audio(val muted: Boolean, val route: Int, val mask: Int)

    /** Rust's view of [calls]; throws if the native library or the answer is bad. */
    fun rust(calls: List<Live>, audio: Audio): CallPlan.View {
        val arr = JSONArray()
        for (c in calls) {
            arr.put(
                JSONObject()
                    .put("state", c.state)
                    .put("handle", c.handle ?: JSONObject.NULL)
                    .put("canHold", c.canHold)
                    .put("name", c.name ?: JSONObject.NULL)
                    .put("jev", c.jev ?: JSONObject.NULL)
                    .put("jevSpam", c.jevSpam),
            )
        }
        val request = JSONObject().put("calls", arr)
            .put("audio", JSONObject().put("muted", audio.muted).put("route", audio.route).put("mask", audio.mask))
        return parse(Calls.ui("callView", request))
    }

    fun parse(o: JSONObject): CallPlan.View {
        val rows = o.getJSONArray("calls").let { a ->
            (0 until a.length()).map { i ->
                val r = a.getJSONObject(i)
                CallPlan.Row(
                    r.getString("peer"),
                    r.optString("number").takeIf { r.has("number") && !r.isNull("number") },
                    r.getString("label"),
                    strings(r.getJSONArray("actions")),
                    r.optString("jev").takeIf { r.has("jev") && !r.isNull("jev") },
                    r.optBoolean("warn"),
                )
            }
        }
        val n = o.optJSONObject("notification")
        val notice = n?.let {
            CallPlan.Notice(
                it.getInt("index"), it.getString("title"), it.getString("text"), it.optBoolean("fullScreen"),
                it.optString("sub").takeIf { _ -> it.has("sub") && !it.isNull("sub") },
                strings(it.getJSONArray("actions")),
            )
        }
        val c = o.getJSONObject("controls")
        val routes = c.getJSONArray("routes").let { a ->
            (0 until a.length()).map { i -> a.getJSONObject(i).let { CallPlan.Route(it.getInt("route"), it.getString("name"), it.getBoolean("on")) } }
        }
        return CallPlan.View(rows, notice, CallPlan.Controls(c.getBoolean("muted"), routes, c.getBoolean("proximity")))
    }

    private fun strings(a: JSONArray) = (0 until a.length()).map { a.getString(it) }
}
