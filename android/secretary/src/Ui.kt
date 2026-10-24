package tech.getsecretary.secretary

import android.app.Activity
import android.graphics.Color
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast

/** Screens built in code: a scrolling column of text and buttons, no resources. */
open class Ui : Activity() {
    lateinit var body: LinearLayout

    fun screen(title: String) {
        val scroll = ScrollView(this)
        scroll.setBackgroundColor(NAVY)
        body = LinearLayout(this)
        body.orientation = LinearLayout.VERTICAL
        body.setPadding(28, 38, 28, 40)
        scroll.addView(body)
        setContentView(scroll)
        text(title, 28).setTextColor(CYAN)
    }

    fun text(value: String, size: Int): TextView {
        val t = TextView(this)
        t.text = value
        t.textSize = size.toFloat()
        t.setTextColor(Color.rgb(224, 231, 245))
        t.setPadding(0, 12, 0, 12)
        body.addView(t)
        return t
    }

    fun button(title: String, action: Runnable): Button {
        val b = Button(this)
        b.text = title
        b.isAllCaps = false
        body.addView(b)
        b.setOnClickListener { action.run() }
        return b
    }

    fun notice(text: String?) {
        Toast.makeText(this, text ?: "", Toast.LENGTH_LONG).show()
    }

    companion object {
        val NAVY = Color.rgb(10, 14, 32)
        val CYAN = Color.rgb(80, 223, 255)
    }
}
