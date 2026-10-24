package tech.getsecretary.secretary

import android.app.AlertDialog
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.widget.TextView
import java.io.ByteArrayOutputStream
import java.io.IOException
import org.json.JSONArray
import org.json.JSONObject

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
        screen("Calls · Transcripts")
        status = text("Private history on this phone · latest 100 calls", 13)
        button("Import a call recording") {
            selectedId = null
            choose()
        }
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
        if (rows.length() == 0) text("No calls recorded yet.", 17)
        for (i in 0 until rows.length()) {
            val r = rows.getJSONObject(i)
            text(r.optString("title"), 21)
            text(r.optString("subtitle"), 12)
            if (r.isNull("transcript")) text(r.optString("note"), 13) else text(r.getString("transcript"), 16).setTextIsSelectable(true)
            val id = r.getString("id")
            button("Attach recording to this call") {
                selectedId = id
                choose()
            }
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
