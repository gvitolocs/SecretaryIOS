package tech.getsecretary.secretary

import android.app.AlertDialog
import android.content.Intent
import android.net.Uri
import android.graphics.drawable.GradientDrawable
import android.os.Bundle
import android.view.Gravity
import android.view.View
import android.widget.LinearLayout
import android.widget.TextView
import java.io.ByteArrayOutputStream
import java.io.IOException
import org.json.JSONArray
import org.json.JSONObject
import tech.getsecretary.design.LucinaTokens.LineHeight
import tech.getsecretary.design.LucinaTokens.TextSize
import tech.getsecretary.design.LucinaTokens.TextWeight

/** The private call history (Rust journal `view`) and recording import (Rust `transcribe`). */
class TranscriptActivity : Ui() {
    private lateinit var status: TextView
    private var selectedId: String? = null

    /** Drops a list that arrives after a newer [render]. */
    private var generation = 0

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        render()
    }

    private fun render() {
        screen("Transcripts")
        status = ui.footnote("Private history on this phone · latest 100 calls").apply {
            setPadding(0, 0, 0, 0)
            layoutParams = ui.lp(bottom = 16f)
        }
        body.addView(status)
        body.addView(ui.primaryButton("Import a call recording") {
            selectedId = null
            choose()
        })
        val mine = ++generation
        val app = applicationContext
        Calls.io.execute {
            val rows = try {
                Calls.view(app)
            } catch (e: Exception) {
                runOnUiThread { if (mine == generation) status.text = e.message }
                null
            }
            if (rows != null) runOnUiThread { if (mine == generation) show(rows) }
        }
    }

    private fun show(rows: JSONArray) {
        if (rows.length() == 0) {
            body.addView(ui.card().apply {
                layoutParams = ui.lp(top = 24f)
                addView(ui.text("No calls recorded yet.", color = ui.c.textSecondary).apply { gravity = Gravity.CENTER })
            })
        }
        for (i in 0 until rows.length()) {
            val r = rows.getJSONObject(i)
            val head = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(ui.dp(16f), ui.dp(12f), ui.dp(16f), ui.dp(12f))
                addView(ui.text(r.optString("title"), TextSize.headline, TextWeight.headline))
                addView(ui.text(r.optString("subtitle"), TextSize.footnote, TextWeight.footnote, ui.c.textSecondary)
                    .apply { layoutParams = ui.lp(top = 2f) })
            }
            val content = LinearLayout(this).apply {
                orientation = LinearLayout.VERTICAL
                setPadding(ui.dp(12f), ui.dp(8f), ui.dp(12f), ui.dp(10f))
            }
            if (r.isNull("transcript")) {
                content.addView(ui.text(r.optString("note"), TextSize.subheadline, TextWeight.subheadline, ui.c.textSecondary,
                    LineHeight.subheadline).apply { setPadding(ui.dp(4f), 0, ui.dp(4f), 0) })
            } else {
                for (turn in r.getString("transcript").split("\n\n")) content.addView(turnBubble(turn))
            }
            val id = r.getString("id")
            val attach = ui.actionRow("Attach recording to this call") {
                selectedId = id
                choose()
            }
            body.addView(ui.group(head, content, attach.view).apply { layoutParams = ui.lp(top = if (i == 0) 28f else 16f) })
        }
    }

    /** One "role: text" paragraph of a transcript as a chat bubble: the owner's on the right in the accent fill. */
    private fun turnBubble(turn: String): View {
        val colon = turn.indexOf(": ")
        val role = if (colon in 1..12) turn.substring(0, colon).lowercase() else ""
        val mine = role == "user"
        val text = if (mine || role == "lucina") turn.substring(colon + 2) else turn
        return ui.bubble(text, mine).apply {
            if (!mine) (background as GradientDrawable).setColor(ui.c.cardRaised)
            setTextIsSelectable(true)
        }
    }

    private fun choose() {
        AlertDialog.Builder(this)
            .setTitle("Transcribe recording")
            .setMessage(
                "Choose an audio recording up to 8 MiB. Secretary sends it to the configured Gemini transcription model. " +
                    "The transcript is kept in your private call history.",
            )
            .setNegativeButton("Cancel", null)
            .setPositiveButton("Choose recording") { _, _ ->
                val i = Intent(Intent.ACTION_OPEN_DOCUMENT).setType("audio/*").addCategory(Intent.CATEGORY_OPENABLE)
                startActivityForResult(i, REQUEST_AUDIO)
            }
            .show()
    }

    @Deprecated("Activity result API; there is no AndroidX here.")
    override fun onActivityResult(request: Int, result: Int, data: Intent?) {
        super.onActivityResult(request, result, data)
        val uri = data?.data
        if (request != REQUEST_AUDIO || result != RESULT_OK || uri == null) return
        status.text = "Transcribing…"
        val id = selectedId
        val files = filesDir.absolutePath
        Thread({
            try {
                val audio = read(uri)
                val args = JSONObject().put("consent", true)
                contentResolver.getType(uri)?.let { args.put("mimeType", it) }
                if (id != null) args.put("id", id)
                unwrap(Native.transcribe(files, Calls.cookie(), args.toString(), audio))
                runOnUiThread { render() }
            } catch (e: Exception) {
                runOnUiThread { status.text = e.message }
            }
        }, "lucina-transcribe").start()
    }

    private fun read(uri: Uri): ByteArray {
        val bytes = ByteArrayOutputStream()
        val input = contentResolver.openInputStream(uri) ?: throw IOException("Recording unavailable.")
        input.use {
            val buffer = ByteArray(8192)
            while (true) {
                val n = it.read(buffer)
                if (n < 0) break
                if (bytes.size() + n > MAX_AUDIO) throw IOException("Recording exceeds 8 MiB.")
                bytes.write(buffer, 0, n)
            }
        }
        return bytes.toByteArray()
    }

    private companion object {
        const val REQUEST_AUDIO = 5
        const val MAX_AUDIO = 8 * 1024 * 1024
    }
}
