package tech.getsecretary.secretary

import android.app.Activity
import android.content.Intent
import android.content.res.Configuration
import android.graphics.ImageDecoder
import android.graphics.Bitmap
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.text.Html
import android.text.InputFilter
import android.text.InputType
import android.text.format.Formatter
import android.util.Size
import android.view.View
import android.view.ViewGroup
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.Toast
import java.io.File
import java.io.FileOutputStream
import java.util.Locale
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject
import tech.getsecretary.design.LucinaUi

/** Framework share intake, URI access and codec bridge. Rust interprets and uploads the manifest. */
class ShareActivity : Activity() {
    private val uris = linkedSetOf<Uri>()
    private val texts = linkedSetOf<String>()
    private var title = ""
    private var sending = false
    private val maxBytes = 2L * 1024 * 1024 * 1024

    override fun onCreate(state: Bundle?) {
        super.onCreate(state)
        // Direct Share and ordinary chooser launches have exactly the same intake path.
        intent.getStringExtra(Intent.EXTRA_SHORTCUT_ID)
        if (intent.action != Intent.ACTION_SEND && intent.action != Intent.ACTION_SEND_MULTIPLE) {
            finish()
            return
        }
        val dark = Configuration(resources.configuration).apply {
            uiMode = (uiMode and Configuration.UI_MODE_NIGHT_MASK.inv()) or Configuration.UI_MODE_NIGHT_YES
        }
        val ui = LucinaUi(createConfigurationContext(dark))
        val root = ui.screen(this)
        window.setLayout(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT)
        root.addView(ui.largeTitle("Lucina"))
        if (Calls.cookie().isBlank()) {
            root.addView(ui.footnote("Open Secretary and sign in first"))
            root.addView(ui.primaryButton("Open") {
                startActivity(Intent(this, MainActivity::class.java))
                finish()
            })
            return
        }
        try {
            collect()
            require(uris.size + texts.size in 1..32) { "Share between 1 and 32 items." }
        } catch (e: Exception) {
            root.addView(ui.footnote(e.message ?: "Could not read this share."))
            return
        }
        val preview = ui.card()
        root.addView(preview)
        for (text in texts) preview.addView(ui.text(text.take(300)).apply { maxLines = 4 })
        for (uri in uris) {
            val row = ui.row()
            val image = ImageView(this).apply { scaleType = ImageView.ScaleType.CENTER_CROP }
            image.setImageResource(android.R.drawable.ic_menu_save)
            row.addView(image, ui.lp(w = ui.dp(56f), h = ui.dp(56f)))
            val label = ui.text("Loading…").apply { maxLines = 3; setPadding(ui.dp(12f), 0, 0, 0) }
            row.addView(label, ui.lp(w = 0, weight = 1f))
            preview.addView(row)
            Thread {
                val info = metadata(uri)
                val thumbnail = try { contentResolver.loadThumbnail(uri, Size(160, 160), null) } catch (e: Exception) { null }
                runOnUiThread {
                    if (!isDestroyed) {
                        label.text = info.name + if (info.size >= 0) "\n" + Formatter.formatShortFileSize(this, info.size) else ""
                        if (thumbnail != null) image.setImageBitmap(thumbnail)
                    } else thumbnail?.recycle()
                }
            }.start()
        }
        val note = ui.pillInput("Add a note for Lucina", InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE).apply {
            isSingleLine = false
            maxLines = 4
            filters = arrayOf(InputFilter.LengthFilter(4000))
        }
        root.addView(note, ui.lp(top = 16f, bottom = 12f))
        val status = ui.footnote("")
        val actions = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(actions)
        fun send(kind: String) {
            if (sending) return
            val fields = 4 + (if (Locale.getDefault().language.isNotEmpty()) 1 else 0) +
                (if (title.isNotEmpty()) 1 else 0) + (if (note.text.isNotEmpty()) 1 else 0)
            if (fields + texts.size + uris.size > 32) {
                status.text = "This share exceeds 32 form parts. Share fewer items at once."
                return
            }
            sending = true
            fun disable(v: View) {
                v.isEnabled = false
                if (v is ViewGroup) for (i in 0 until v.childCount) disable(v.getChildAt(i))
            }
            disable(actions)
            note.isEnabled = false
            status.text = "Preparing share…"
            val words = note.text.toString()
            Thread {
                var folder: File? = null
                try {
                    val id = UUID.randomUUID().toString()
                    folder = File(cacheDir, "shares/$id")
                    check(folder.mkdirs()) { "Share storage unavailable." }
                    val files = JSONArray()
                    var total = 0L
                    for ((index, uri) in uris.withIndex()) {
                        val info = metadata(uri)
                        check(info.size < 0 || info.size <= maxBytes - total) { "Share exceeds 2 GB." }
                        var mime = contentResolver.getType(uri) ?: "application/octet-stream"
                        var name = info.name
                        val heic = mime.lowercase(Locale.ROOT) in listOf("image/heic", "image/heif", "image/heic-sequence", "image/heif-sequence") ||
                            name.lowercase(Locale.ROOT).let { it.endsWith(".heic") || it.endsWith(".heif") }
                        if (heic) {
                            name = name.substringBeforeLast('.', name) + ".jpg"
                            mime = "image/jpeg"
                        }
                        val safe = name.map { if (it.isLetterOrDigit() || it in ".-_ ") it else '_' }.joinToString("").take(100).ifBlank { "file" }
                        val target = File(folder, "%02d-%s".format(Locale.ROOT, index, safe))
                        if (heic) {
                            val bitmap = ImageDecoder.decodeBitmap(ImageDecoder.createSource(contentResolver, uri)) { decoder, _, _ ->
                                decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
                            }
                            try {
                                FileOutputStream(target).use { out ->
                                    check(bitmap.compress(Bitmap.CompressFormat.JPEG, 92, out)) { "Could not convert HEIC image." }
                                    out.fd.sync()
                                }
                            } finally { bitmap.recycle() }
                            total += target.length()
                            check(total <= maxBytes) { "Share exceeds 2 GB." }
                        } else {
                            contentResolver.openInputStream(uri).use { input ->
                                checkNotNull(input) { "Could not open $name." }
                                FileOutputStream(target).use { out ->
                                    val buffer = ByteArray(64 * 1024)
                                    while (true) {
                                        val count = input.read(buffer)
                                        if (count < 0) break
                                        total += count
                                        check(total <= maxBytes) { "Share exceeds 2 GB." }
                                        out.write(buffer, 0, count)
                                    }
                                    out.fd.sync()
                                }
                            }
                        }
                        files.put(JSONObject().put("path", target.absolutePath).put("name", name).put("mime", mime))
                    }
                    val manifest = JSONObject().put("shareId", id).put("intent", kind).put("note", words)
                        .put("lang", Locale.getDefault().language).put("title", title)
                        .put("texts", JSONArray(texts.toList())).put("urls", JSONArray()).put("files", files)
                    val bytes = manifest.toString().toByteArray(Charsets.UTF_8)
                    check(bytes.size <= 1024 * 1024) { "Shared text exceeds limit." }
                    val path = File(folder, "manifest.json")
                    FileOutputStream(path).use { it.write(bytes); it.fd.sync() }
                    ShareJobService.schedule(this, path)
                    runOnUiThread {
                        ShareShortcut.publish(this, used = true)
                        Toast.makeText(this, "Sent to Lucina", Toast.LENGTH_SHORT).show()
                        finish()
                    }
                } catch (e: Exception) {
                    folder?.deleteRecursively()
                    runOnUiThread {
                        sending = false
                        fun enable(v: View) {
                            v.isEnabled = true
                            if (v is ViewGroup) for (i in 0 until v.childCount) enable(v.getChildAt(i))
                        }
                        enable(actions)
                        note.isEnabled = true
                        status.text = e.message ?: "Could not prepare this share. Try again."
                    }
                }
            }.start()
        }
        actions.addView(ui.primaryButton("Send to Lucina") { send("ask") })
        fun chip(label: String, kind: String) = ui.chip(label, ui.c.accent).apply {
            minimumHeight = ui.dp(44f)
            gravity = android.view.Gravity.CENTER
            isClickable = true
            setOnClickListener { send(kind) }
        }
        actions.addView(ui.pair(chip("Save to library", "library"), chip("Add to shopping", "shopping")), ui.lp(top = 8f))
        actions.addView(ui.pair(chip("Save as note", "note"), chip("Remind me", "remind")), ui.lp(top = 8f))
        root.addView(status)
    }

    @Suppress("DEPRECATION")
    private fun collect() {
        fun addUri(uri: Uri) {
            if (uri.scheme == "content") uris.add(uri)
            else texts.add(uri.toString())
        }
        title = intent.getCharSequenceExtra(Intent.EXTRA_SUBJECT)?.toString() ?: ""
        val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()
        if (text != null) texts.add(text)
        else intent.getStringExtra(Intent.EXTRA_HTML_TEXT)?.let { texts.add(Html.fromHtml(it, Html.FROM_HTML_MODE_LEGACY).toString()) }
        if (intent.action == Intent.ACTION_SEND_MULTIPLE) {
            intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM)?.forEach { addUri(it) }
        } else intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM)?.let { addUri(it) }
        intent.clipData?.let { clip ->
            for (i in 0 until clip.itemCount) {
                val item = clip.getItemAt(i)
                item.uri?.let { addUri(it) }
                if (item.text != null) texts.add(item.text.toString())
                else if (text == null) item.htmlText?.let { texts.add(Html.fromHtml(it, Html.FROM_HTML_MODE_LEGACY).toString()) }
            }
        }
        texts.removeAll { it.isBlank() }
    }

    private data class Info(val name: String, val size: Long)

    private fun metadata(uri: Uri): Info {
        var name = "Shared file"
        var size = -1L
        try {
            contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE), null, null, null)?.use { c ->
                if (c.moveToFirst()) {
                    val n = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                    val s = c.getColumnIndex(OpenableColumns.SIZE)
                    if (n >= 0 && !c.isNull(n)) name = c.getString(n)
                    if (s >= 0 && !c.isNull(s)) size = c.getLong(s)
                }
            }
        } catch (e: Exception) {
        }
        return Info(name, size)
    }
}
