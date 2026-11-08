package tech.getsecretary.secretary

import android.Manifest
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.provider.ContactsContract
import android.text.Editable
import android.text.TextWatcher
import android.widget.EditText
import android.widget.LinearLayout
import tech.getsecretary.design.LucinaTokens.TextSize
import tech.getsecretary.design.LucinaTokens.TextWeight

/** Contacts: search by name or number, tap to call (READ_CONTACTS, which the phone role grants). Read on this phone only. */
class ContactsActivity : Ui() {
    private lateinit var search: EditText
    private lateinit var list: LinearLayout
    private var generation = 0

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        screen("Contacts")
        search = ui.pillInput("Search contacts").apply { layoutParams = ui.lp(top = 8f, bottom = 12f) }
        search.addTextChangedListener(object : TextWatcher {
            override fun afterTextChanged(s: Editable?) = load()
            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}
        })
        body.addView(search)
        list = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; layoutParams = ui.lp() }
        body.addView(list)
        if (checkSelfPermission(Manifest.permission.READ_CONTACTS) != PackageManager.PERMISSION_GRANTED) {
            list.addView(ui.footnote("Allow contacts to search them here. They are only read on this phone."))
            list.addView(ui.primaryButton("Allow contacts") {
                requestPermissions(arrayOf(Manifest.permission.READ_CONTACTS), 40)
            }.apply { layoutParams = ui.lp(top = 16f) })
        } else {
            load()
        }
    }

    private fun load() {
        val q = search.text.toString().trim()
        val mine = ++generation
        val app = applicationContext
        Calls.io.execute {
            val found = safely(emptyList<Pair<String, String>>()) { query(app, q) }
            runOnUiThread { if (!isDestroyed && mine == generation) show(found, q) }
        }
    }

    private fun query(c: android.content.Context, q: String): List<Pair<String, String>> {
        val uri = if (q.isEmpty()) ContactsContract.CommonDataKinds.Phone.CONTENT_URI
        else Uri.withAppendedPath(ContactsContract.CommonDataKinds.Phone.CONTENT_FILTER_URI, Uri.encode(q))
        val out = ArrayList<Pair<String, String>>()
        val seen = HashSet<String>()
        c.contentResolver.query(
            uri,
            arrayOf(ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME, ContactsContract.CommonDataKinds.Phone.NUMBER),
            null, null, ContactsContract.CommonDataKinds.Phone.SORT_KEY_PRIMARY,
        )?.use {
            while (it.moveToNext() && out.size < 100) {
                val name = it.getString(0) ?: continue
                val number = it.getString(1) ?: continue
                if (seen.add(name + DialPlan.normalize(number))) out.add(name to number)
            }
        }
        return out
    }

    private fun show(found: List<Pair<String, String>>, q: String) {
        list.removeAllViews()
        if (found.isEmpty()) {
            list.addView(ui.footnote(if (q.isEmpty()) "No contacts." else "No contact matches “$q”."))
            return
        }
        val rows = found.map { (name, number) ->
            ui.row().apply {
                orientation = LinearLayout.VERTICAL
                addView(ui.text(name, TextSize.body, TextWeight.body, ui.c.text))
                addView(ui.text(number, TextSize.footnote, TextWeight.footnote, ui.c.textSecondary))
                isClickable = true
                setOnClickListener { call(number) }
            }
        }
        list.addView(ui.group(*rows.toTypedArray()))
    }

    private fun call(number: String) {
        if (checkSelfPermission(Manifest.permission.CALL_PHONE) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.CALL_PHONE), 41)
            return
        }
        if (Phone.dial(this, number) is DialPlan.Outcome.Invalid) notice("That number cannot be called.")
    }

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        if (r == 40 && g.isNotEmpty() && g[0] == PackageManager.PERMISSION_GRANTED) {
            list.removeAllViews()
            load()
        }
    }
}
