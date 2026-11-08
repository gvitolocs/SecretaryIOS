package tech.getsecretary.secretary

import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.net.Uri
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextPaint
import android.text.TextWatcher
import android.text.method.LinkMovementMethod
import android.text.style.BackgroundColorSpan
import android.text.style.ClickableSpan
import android.text.style.ForegroundColorSpan
import android.text.style.StrikethroughSpan
import android.text.style.StyleSpan
import android.text.style.TypefaceSpan
import android.view.MotionEvent
import android.view.View
import android.widget.EditText
import android.widget.HorizontalScrollView
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import java.util.concurrent.Executors
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min
import org.json.JSONArray
import org.json.JSONObject

/**
 * An Obsidian vault on the phone: a folder of plain Markdown in this app's external files (`.../files/Vault`), read by the same Rust
 * core as Juno and iOS (docs/lucina-vault.md). Kotlin holds no vault logic. One activity per place: a folder, a note, search, tags, graph.
 */
object Vault {
    private val io = Executors.newSingleThreadExecutor()
    private val main = Handler(Looper.getMainLooper())

    fun root(c: Context): String = java.io.File(c.getExternalFilesDir(null), "Vault").absolutePath

    /** One request of the protocol off the main thread; [then] gets the answer on it. */
    fun ask(c: Context, q: JSONObject, then: (JSONObject) -> Unit) {
        val r = root(c)
        io.execute {
            val out = try {
                JSONObject(Native.vault(r, q.toString()))
            } catch (e: Exception) {
                JSONObject().put("ok", false).put("code", "bridge").put("error", e.message ?: "The vault did not answer.")
            }
            main.post { then(out) }
        }
    }

    fun nameOf(path: String): String {
        val base = path.substringAfterLast('/')
        return if (base.endsWith(".md", ignoreCase = true)) base.dropLast(3) else base
    }
}

class VaultActivity : Ui() {
    private var place = "folder"
    private var arg = ""
    private var note: JSONObject? = null

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        place = intent.getStringExtra("place") ?: "folder"
        arg = intent.getStringExtra("arg") ?: ""
        when (place) {
            "note" -> showNote()
            "tags" -> showTags()
            "graph" -> showGraph()
            "search" -> showSearch()
            else -> showFolder()
        }
    }

    private fun go(place: String, arg: String = "") {
        startActivity(Intent(this, VaultActivity::class.java).putExtra("place", place).putExtra("arg", arg))
    }

    private fun row(title: String, detail: String = "", open: (() -> Unit)?): View = ui.valueRow(title, detail, chevron = open != null, onClick = open).view

    private fun showFolder() {
        screen(if (arg.isEmpty()) "Vault" else Vault.nameOf(arg))
        if (arg.isEmpty() && !java.io.File(Vault.root(this)).isDirectory) {
            body.addView(ui.footnote("No vault on this phone yet. A vault is a folder of Markdown files, the one Obsidian opens. It goes here:\n${Vault.root(this)}"))
            return
        }
        if (arg.isEmpty()) {
            Vault.ask(this, JSONObject().put("op", "status")) { s ->
                if (!s.optBoolean("ok")) {
                    body.addView(ui.footnote(s.optString("error")))
                    return@ask
                }
                val c = s.getJSONObject("counts")
                body.addView(ui.footnote("${c.getInt("notes")} notes, ${c.getInt("links")} links, ${c.getInt("unresolved")} unresolved"))
                body.addView(ui.group(row("Search", "", { go("search") }), row("Tags", "", { go("tags") }), row("Graph", "", { go("graph") })))
                list()
            }
        } else {
            list()
        }
    }

    private fun list() {
        Vault.ask(this, JSONObject().put("op", "list").put("folder", arg)) { r ->
            if (!r.optBoolean("ok")) {
                body.addView(ui.footnote(r.optString("error")))
                return@ask
            }
            val folders = r.getJSONArray("folders")
            if (folders.length() > 0) {
                body.addView(ui.sectionHeader("Folders"))
                body.addView(ui.group(*Array(folders.length()) { i ->
                    val f = folders.getString(i)
                    row(Vault.nameOf(f), "", { go("folder", f) })
                }))
            }
            val notes = r.getJSONArray("notes")
            if (notes.length() > 0) {
                body.addView(ui.sectionHeader("Notes"))
                body.addView(ui.group(*Array(notes.length()) { i ->
                    val n = notes.getJSONObject(i)
                    row(n.getString("name"), "", { go("note", n.getString("path")) })
                }))
            }
            val files = r.getJSONArray("files")
            if (files.length() > 0) {
                body.addView(ui.sectionHeader("Files"))
                body.addView(ui.group(*Array(files.length()) { i -> row(Vault.nameOf(files.getString(i)), "", null) }))
            }
        }
    }

    private fun showSearch() {
        screen("Search")
        val field = ui.pillInput("Words, \"a phrase\", -not, tag:x, path:x")
        body.addView(field)
        val results = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        body.addView(results)
        val handler = Handler(Looper.getMainLooper())
        var asked = 0
        fun search(q: String) {
            val mine = ++asked
            results.removeAllViews()
            if (q.isBlank()) return
            Vault.ask(this, JSONObject().put("op", "search").put("query", q).put("limit", 100)) { r ->
                if (mine != asked) return@ask
                val hits = r.optJSONArray("hits") ?: JSONArray()
                if (hits.length() == 0) results.addView(ui.footnote("Nothing found."))
                for (i in 0 until hits.length()) {
                    val h = hits.getJSONObject(i)
                    val lines = h.getJSONArray("lines")
                    val line = if (lines.length() > 0) lines.getJSONArray(0).getString(1) else ""
                    val t = ui.text(h.getString("name"))
                    val d = ui.footnote(line)
                    val box = ui.card().apply {
                        addView(t)
                        addView(d)
                        setOnClickListener { go("note", h.getString("path")) }
                    }
                    results.addView(box, ui.lp(top = 6f))
                }
            }
        }
        if (arg.isNotEmpty()) {
            field.setText(arg)
            search(arg)
        }
        field.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun afterTextChanged(s: Editable?) {
                handler.removeCallbacksAndMessages(null)
                val q = s?.toString() ?: ""
                handler.postDelayed({ search(q) }, 250)
            }
        })
    }

    private fun showTags() {
        screen("Tags")
        Vault.ask(this, JSONObject().put("op", "tags")) { r ->
            val tags = r.optJSONArray("tags") ?: JSONArray()
            body.addView(ui.group(*Array(tags.length()) { i ->
                val t = tags.getJSONObject(i)
                val tag = t.getString("tag")
                row("#$tag", t.getInt("count").toString(), { go("search", "tag:$tag") })
            }))
        }
    }

    private fun showNote() {
        screen(Vault.nameOf(arg))
        Vault.ask(this, JSONObject().put("op", "note").put("path", arg)) { n ->
            if (!n.optBoolean("ok")) {
                body.addView(ui.footnote(n.optString("error")))
                return@ask
            }
            note = n
            drawNote(n)
        }
    }

    private fun drawNote(n: JSONObject) {
        val tags = n.getJSONArray("tags")
        if (tags.length() > 0) {
            val seen = LinkedHashSet<String>()
            for (i in 0 until tags.length()) seen.add("#" + tags.getString(i))
            body.addView(ui.text(seen.joinToString("  "), 13f, color = ui.c.accent))
        }
        blocks(n.getJSONArray("blocks"), body, 0)
        val back = n.getJSONArray("backlinks")
        if (back.length() > 0) {
            body.addView(ui.sectionHeader("Backlinks (${back.length()})"))
            val seen = LinkedHashSet<String>()
            for (i in 0 until back.length()) seen.add(back.getJSONObject(i).getString("from"))
            body.addView(ui.group(*seen.map { p -> row(Vault.nameOf(p), "", { go("note", p) }) }.toTypedArray()))
        }
        body.addView(ui.group(
            row("Graph around this note", "", { go("graph", arg) }),
            row("Edit", "", { edit() }),
        ), ui.lp(top = 16f))
    }

    /** Remote pictures are never fetched, so a note cannot tell a server it was opened. */
    private fun blocks(list: JSONArray, into: LinearLayout, depth: Int) {
        for (i in 0 until list.length()) {
            val b = list.getJSONObject(i)
            when (b.getString("type")) {
                "heading" -> {
                    val size = when (b.getInt("level")) { 1 -> 24f; 2 -> 20f; 3 -> 18f; else -> 16f }
                    into.addView(textOf(b.getJSONArray("spans"), size, true), ui.lp(top = 12f))
                }
                "paragraph" -> into.addView(textOf(b.getJSONArray("spans"), 16f, false), ui.lp(top = 4f))
                "code", "math", "html" -> {
                    val t = ui.text(b.getString("text").trimEnd(), 13f).apply { typeface = Typeface.MONOSPACE; setPadding(ui.dp(8f), ui.dp(8f), ui.dp(8f), ui.dp(8f)) }
                    into.addView(HorizontalScrollView(this).apply { setBackgroundColor(ui.c.card); addView(t) }, ui.lp(top = 4f))
                }
                "quote" -> into.addView(quoted(b.getJSONArray("blocks"), depth), ui.lp(top = 4f))
                "callout" -> {
                    val box = ui.card()
                    val title = b.getJSONArray("title")
                    if (title.length() > 0) box.addView(textOf(title, 16f, true)) else box.addView(ui.text(b.getString("kind"), 16f))
                    blocks(b.getJSONArray("blocks"), box, depth)
                    into.addView(box, ui.lp(top = 6f))
                }
                "list" -> {
                    val items = b.getJSONArray("items")
                    val start = b.getInt("start")
                    val ordered = b.getBoolean("ordered")
                    for (k in 0 until items.length()) {
                        val item = items.getJSONObject(k)
                        val line = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
                        val mark = when {
                            !item.isNull("task") -> if (item.getBoolean("task")) "☑" else "☐"
                            ordered -> "${start + k}."
                            else -> "•"
                        }
                        line.addView(ui.text(mark, 16f, color = ui.c.accent), LinearLayout.LayoutParams(ui.dp(26f), -2))
                        val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
                        blocks(item.getJSONArray("blocks"), col, depth + 1)
                        line.addView(col, LinearLayout.LayoutParams(0, -2, 1f))
                        into.addView(line, ui.lp(top = 2f))
                    }
                }
                "table" -> into.addView(table(b), ui.lp(top = 6f))
                "rule" -> into.addView(View(this).apply { setBackgroundColor(ui.c.separator) }, ui.lp(h = 1).apply { topMargin = ui.dp(10f); bottomMargin = ui.dp(10f) })
                "embed" -> into.addView(embed(b), ui.lp(top = 4f))
                "footnoteDefinition" -> {
                    into.addView(ui.footnote("[${b.getString("label")}]"))
                    blocks(b.getJSONArray("blocks"), into, depth + 1)
                }
            }
        }
    }

    private fun quoted(list: JSONArray, depth: Int): View {
        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        row.addView(View(this).apply { setBackgroundColor(ui.c.separator) }, LinearLayout.LayoutParams(ui.dp(3f), -1).apply { rightMargin = ui.dp(10f) })
        val col = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        blocks(list, col, depth)
        row.addView(col, LinearLayout.LayoutParams(0, -2, 1f))
        return row
    }

    private fun table(b: JSONObject): View {
        val grid = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        fun line(cells: JSONArray, head: Boolean) {
            val r = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
            for (c in 0 until cells.length()) {
                r.addView(textOf(cells.getJSONArray(c), 14f, head), LinearLayout.LayoutParams(ui.dp(170f), -2).apply { rightMargin = ui.dp(8f) })
            }
            grid.addView(r, ui.lp(top = 3f))
        }
        line(b.getJSONArray("head"), true)
        val rows = b.getJSONArray("rows")
        for (i in 0 until rows.length()) line(rows.getJSONArray(i), false)
        return HorizontalScrollView(this).apply { addView(grid) }
    }

    private fun embed(b: JSONObject): View {
        val target = b.getString("target")
        if (b.getBoolean("external")) return ui.footnote("Remote picture not loaded: $target")
        val holder = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        val label = ui.footnote("Embedded: $target").apply { setOnClickListener { follow(target) } }
        holder.addView(label)
        Vault.ask(this, JSONObject().put("op", "resolve").put("from", arg).put("link", target)) { r ->
            val dest = r.optString("dest", "")
            val picture = dest.substringAfterLast('.').lowercase() in setOf("png", "jpg", "jpeg", "webp", "gif")
            if (dest.isEmpty() || !picture) return@ask
            Vault.ask(this, JSONObject().put("op", "file_path").put("path", dest)) { f ->
                val path = f.optString("absolute", "")
                if (path.isEmpty()) return@ask
                Thread {
                    val o = BitmapFactory.Options().apply { inSampleSize = 2 }
                    val bmp = BitmapFactory.decodeFile(path, o)
                    if (bmp != null) {
                        runOnUiThread {
                            holder.removeAllViews()
                            holder.addView(ImageView(this).apply { setImageBitmap(bmp); adjustViewBounds = true })
                        }
                    }
                }.start()
            }
        }
        return holder
    }

    private fun textOf(spans: JSONArray, size: Float, bold: Boolean): TextView {
        val out = SpannableStringBuilder()
        for (i in 0 until spans.length()) {
            val s = spans.getJSONObject(i)
            val at = out.length
            out.append(s.getString("text"))
            val end = out.length
            fun mark(what: Any) = out.setSpan(what, at, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            if (s.optBoolean("bold")) mark(StyleSpan(Typeface.BOLD))
            if (s.optBoolean("italic")) mark(StyleSpan(Typeface.ITALIC))
            if (s.optBoolean("strike")) mark(StrikethroughSpan())
            if (s.optBoolean("code")) mark(TypefaceSpan("monospace"))
            if (s.optBoolean("mark")) mark(BackgroundColorSpan(ui.c.accentSoft))
            if (s.has("tag")) mark(ForegroundColorSpan(ui.c.accent))
            if (s.has("link")) {
                val link = s.getString("link")
                val external = s.optBoolean("external")
                mark(object : ClickableSpan() {
                    override fun onClick(v: View) {
                        if (external) {
                            try {
                                startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(link)))
                            } catch (e: Exception) {
                                notice(link)
                            }
                        } else {
                            follow(link)
                        }
                    }

                    override fun updateDrawState(ds: TextPaint) {
                        ds.color = ui.c.accent
                        ds.isUnderlineText = true
                    }
                })
            }
        }
        return ui.text(out, size).apply {
            if (bold) setTypeface(typeface, Typeface.BOLD)
            movementMethod = LinkMovementMethod.getInstance()
            setTextIsSelectable(false)
        }
    }

    private fun follow(link: String) {
        Vault.ask(this, JSONObject().put("op", "resolve").put("from", arg).put("link", link)) { r ->
            val dest = if (r.isNull("dest")) "" else r.optString("dest", "")
            when {
                dest.isNotEmpty() && r.optBoolean("isNote") -> if (dest != arg) go("note", dest)
                dest.isNotEmpty() -> notice("$dest is a file, not a note.")
                else -> notice("No note called $link yet.")
            }
        }
    }

    /** Read-only until allowed; a save names the revision it read, and a conflict keeps both as notes. */
    private fun edit() {
        Vault.ask(this, JSONObject().put("op", "status")) { s ->
            if (s.optBoolean("writable")) {
                editor()
                return@ask
            }
            AlertDialog.Builder(this)
                .setTitle("This vault is read-only here")
                .setMessage("Saves never overwrite a change they have not seen, and a conflict keeps both as notes.")
                .setPositiveButton("Allow editing") { _, _ ->
                    Vault.ask(this, JSONObject().put("op", "set_writable").put("writable", true)) { editor() }
                }
                .setNegativeButton("Stay read-only", null)
                .show()
        }
    }

    private fun editor() {
        val n = note ?: return
        screen("Edit")
        val field = EditText(this).apply {
            setText(n.getString("text"))
            setTextColor(ui.c.text)
            textSize = 15f
            typeface = Typeface.MONOSPACE
        }
        body.addView(ui.primaryButton("Save") { save(n, field.text.toString()) })
        body.addView(field, ui.lp(top = 8f))
    }

    private fun save(n: JSONObject, text: String) {
        val q = JSONObject().put("op", "save").put("path", arg).put("text", text).put("revision", n.getString("revision"))
        Vault.ask(this, q) { r ->
            when {
                r.optBoolean("ok") -> {
                    finish()
                    go("note", arg)
                }
                r.optString("code") == "conflict" -> AlertDialog.Builder(this)
                    .setTitle("This note changed on disk")
                    .setMessage("Something else wrote it after you opened it. Nothing was overwritten.")
                    .setPositiveButton("Keep mine as a new note") { _, _ -> keepCopy(text) }
                    .setNegativeButton("Drop mine, read theirs") { _, _ ->
                        finish()
                        go("note", arg)
                    }
                    .show()
                else -> notice(r.optString("error"))
            }
        }
    }

    private fun keepCopy(text: String) {
        val date = java.text.SimpleDateFormat("yyyy-MM-dd", java.util.Locale.ROOT).format(java.util.Date())
        val q = JSONObject().put("op", "conflict_copy").put("path", arg).put("text", text).put("date", date).put("device", "android")
        Vault.ask(this, q) { r ->
            if (r.optBoolean("ok")) {
                finish()
                go("note", r.getString("path"))
            } else {
                notice(r.optString("error"))
            }
        }
    }

    private fun showGraph() {
        ui = tech.getsecretary.design.LucinaUi(this)
        val view = GraphView(this, ui.c.separator, ui.c.accent, ui.c.textSecondary, arg) { go("note", it) }
        view.setBackgroundColor(ui.c.bg)
        setContentView(view)
        val q = JSONObject().put("op", "graph")
        if (arg.isNotEmpty()) q.put("around", arg).put("depth", 2)
        Vault.ask(this, q) { r -> if (r.optBoolean("ok")) view.show(r) else notice(r.optString("error")) }
    }
}

/** The note graph: dragged with one finger, pinched with two. A tap opens the nearest note. */
class GraphView(
    c: Context,
    private val edgeColor: Int,
    private val nodeColor: Int,
    private val textColor: Int,
    private val hot: String,
    private val open: (String) -> Unit,
) : View(c) {
    private var xs = FloatArray(0)
    private var ys = FloatArray(0)
    private var degree = IntArray(0)
    private var paths = emptyArray<String>()
    private var names = emptyArray<String>()
    private var edges = IntArray(0)
    private var fit = 1f
    private var scale = 1f
    private var dx = 0f
    private var dy = 0f
    private var lastX = 0f
    private var lastY = 0f
    private var lastSpan = 0f
    private var moved = 0f
    private val edge = Paint().apply { color = edgeColor; strokeWidth = 1f; isAntiAlias = true }
    private val node = Paint().apply { color = nodeColor; isAntiAlias = true }
    private val label = Paint().apply { color = textColor; textSize = 26f; isAntiAlias = true }

    fun show(g: JSONObject) {
        val nodes = g.getJSONArray("nodes")
        val n = nodes.length()
        xs = FloatArray(n)
        ys = FloatArray(n)
        degree = IntArray(n)
        paths = Array(n) { "" }
        names = Array(n) { "" }
        var far = 1f
        for (i in 0 until n) {
            val o = nodes.getJSONObject(i)
            xs[i] = o.getDouble("x").toFloat()
            ys[i] = o.getDouble("y").toFloat()
            degree[i] = o.getInt("degree")
            paths[i] = o.getString("path")
            names[i] = o.getString("name")
            far = max(far, max(kotlin.math.abs(xs[i]), kotlin.math.abs(ys[i])))
        }
        val es = g.getJSONArray("edges")
        edges = IntArray(es.length() * 2)
        for (i in 0 until es.length()) {
            val e = es.getJSONArray(i)
            edges[2 * i] = e.getInt(0)
            edges[2 * i + 1] = e.getInt(1)
        }
        fit = far
        invalidate()
    }

    private fun zoom() = min(width, height) / 2f / fit * 0.9f * scale
    private fun px(i: Int) = xs[i] * zoom() + width / 2f + dx
    private fun py(i: Int) = ys[i] * zoom() + height / 2f + dy

    override fun onDraw(canvas: Canvas) {
        var i = 0
        while (i < edges.size) {
            canvas.drawLine(px(edges[i]), py(edges[i]), px(edges[i + 1]), py(edges[i + 1]), edge)
            i += 2
        }
        for (k in xs.indices) {
            val r = max(2f, (2f + kotlin.math.log2(degree[k] + 1f) * 1.2f) * min(1.6f, scale))
            canvas.drawCircle(px(k), py(k), r, node)
            if (paths[k] == hot || degree[k] > 30 || (scale > 2.5f && degree[k] > 3) || xs.size < 60) {
                canvas.drawText(names[k].take(32), px(k) + r + 4, py(k) + 8, label)
            }
        }
    }

    override fun onTouchEvent(e: MotionEvent): Boolean {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                lastX = e.x
                lastY = e.y
                moved = 0f
            }
            MotionEvent.ACTION_POINTER_DOWN -> lastSpan = span(e)
            MotionEvent.ACTION_MOVE -> {
                if (e.pointerCount >= 2) {
                    val s = span(e)
                    if (lastSpan > 0) scale = max(0.3f, min(14f, scale * s / lastSpan))
                    lastSpan = s
                    moved += 100f
                } else {
                    dx += e.x - lastX
                    dy += e.y - lastY
                    moved += hypot(e.x - lastX, e.y - lastY)
                    lastX = e.x
                    lastY = e.y
                }
                invalidate()
            }
            MotionEvent.ACTION_UP -> if (moved < 12f) {
                var best = -1
                var bestD = 60f
                for (k in xs.indices) {
                    val d = hypot(px(k) - e.x, py(k) - e.y)
                    if (d < bestD) {
                        bestD = d
                        best = k
                    }
                }
                if (best >= 0) open(paths[best])
            }
        }
        return true
    }

    private fun span(e: MotionEvent) = hypot(e.getX(0) - e.getX(1), e.getY(0) - e.getY(1))
}
