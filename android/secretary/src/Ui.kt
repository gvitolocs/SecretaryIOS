package tech.getsecretary.secretary

import android.app.Activity
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Toast
import tech.getsecretary.design.LucinaUi

/** Screens built in code in the Lucina look (android/common/LucinaUi.kt): a scrolling column, no resources. */
open class Ui : Activity() {
    lateinit var ui: LucinaUi
    lateinit var body: LinearLayout

    /** A fresh page with the large bold `title`; fills [body]. */
    fun screen(title: String) {
        ui = LucinaUi(this)
        body = ui.screen(this)
        body.addView(ui.largeTitle(title))
    }

    /** Scrolls the page to its end (after a new chat bubble). */
    fun scrollToEnd() {
        val scroll = body.parent as? ScrollView ?: return
        scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }

    /** A [Glyph] icon of `size` dp. */
    fun icon(kind: Glyph.Kind, color: Int, size: Float = 28f) = ImageView(this).apply {
        setImageDrawable(Glyph(kind, color, ui.dp(size)))
    }

    fun notice(text: String?) {
        Toast.makeText(this, text ?: "", Toast.LENGTH_LONG).show()
    }
}
