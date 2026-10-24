package tech.getsecretary.design

import android.app.Activity
import android.content.Context
import android.content.res.ColorStateList
import android.content.res.Configuration
import android.graphics.Typeface
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.RippleDrawable
import android.os.Build
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewOutlineProvider
import android.view.WindowInsets
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import tech.getsecretary.design.LucinaTokens.Space
import tech.getsecretary.design.LucinaTokens.TextSize
import tech.getsecretary.design.LucinaTokens.TextWeight
import tech.getsecretary.design.LucinaTokens.LineHeight

/** The token colours for one appearance (LucinaTokens.Dark or .Light). */
class LucinaColors(dark: Boolean) {
    val bg = if (dark) LucinaTokens.Dark.bg else LucinaTokens.Light.bg
    val card = if (dark) LucinaTokens.Dark.card else LucinaTokens.Light.card
    val cardRaised = if (dark) LucinaTokens.Dark.cardRaised else LucinaTokens.Light.cardRaised
    val field = if (dark) LucinaTokens.Dark.field else LucinaTokens.Light.field
    val separator = if (dark) LucinaTokens.Dark.separator else LucinaTokens.Light.separator
    val text = if (dark) LucinaTokens.Dark.text else LucinaTokens.Light.text
    val textSecondary = if (dark) LucinaTokens.Dark.textSecondary else LucinaTokens.Light.textSecondary
    val textTertiary = if (dark) LucinaTokens.Dark.textTertiary else LucinaTokens.Light.textTertiary
    val accent = if (dark) LucinaTokens.Dark.accent else LucinaTokens.Light.accent
    val accentFill = if (dark) LucinaTokens.Dark.accentFill else LucinaTokens.Light.accentFill
    val accentSoft = if (dark) LucinaTokens.Dark.accentSoft else LucinaTokens.Light.accentSoft
    val onAccent = if (dark) LucinaTokens.Dark.onAccent else LucinaTokens.Light.onAccent
    val positive = if (dark) LucinaTokens.Dark.positive else LucinaTokens.Light.positive
    val negative = if (dark) LucinaTokens.Dark.negative else LucinaTokens.Light.negative
    val warning = if (dark) LucinaTokens.Dark.warning else LucinaTokens.Light.warning
    val info = if (dark) LucinaTokens.Dark.info else LucinaTokens.Light.info
}

/** A label, an optional status dot and a secondary value; `set` updates the last two. */
class ValueRow(val view: LinearLayout, val label: TextView, val dot: View, val value: TextView) {
    fun set(text: CharSequence, dotColor: Int? = null) {
        value.text = text
        dot.visibility = if (dotColor == null) View.GONE else View.VISIBLE
        if (dotColor != null) (dot.background as GradientDrawable).setColor(dotColor)
    }
}

/**
 * Builders for framework-only screens in the Lucina look (iOS Weight/Settings): black page, #1C1C1E cards
 * of radius 18, periwinkle accent, large bold title, grouped inset rows, pill buttons. Views come back with
 * LinearLayout params set, so `column.addView(ui.card(...))` is enough.
 */
class LucinaUi(private val ctx: Context) {
    val dark = ctx.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK == Configuration.UI_MODE_NIGHT_YES
    val c = LucinaColors(dark)
    private val dm = ctx.resources.displayMetrics

    fun dp(v: Float): Int = Math.round(TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, dm))
    fun sp(v: Float): Float = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, v, dm)

    fun font(weight: Int): Typeface = if (Build.VERSION.SDK_INT >= 28) Typeface.create(Typeface.DEFAULT, weight, false)
        else if (weight >= 600) Typeface.DEFAULT_BOLD else Typeface.DEFAULT

    fun text(s: CharSequence, size: Float = TextSize.body, weight: Int = TextWeight.body, color: Int = c.text, line: Float = 0f) =
        TextView(ctx).apply {
            text = s
            setTextSize(TypedValue.COMPLEX_UNIT_SP, size)
            typeface = font(weight)
            setTextColor(color)
            if (line > 0f && Build.VERSION.SDK_INT >= 28) lineHeight = Math.round(sp(line))
        }

    fun lp(w: Int = -1, h: Int = -2, top: Float = 0f, bottom: Float = 0f, weight: Float = 0f) =
        LinearLayout.LayoutParams(w, h, weight).apply { topMargin = dp(top); bottomMargin = dp(bottom) }

    fun shape(color: Int, radius: Float, oval: Boolean = false) = GradientDrawable().apply {
        setColor(color)
        if (oval) shape = GradientDrawable.OVAL else cornerRadius = dp(radius).toFloat()
    }

    private fun ripple(content: android.graphics.drawable.Drawable?, mask: android.graphics.drawable.Drawable? = null) =
        RippleDrawable(ColorStateList.valueOf(if (dark) 0x33FFFFFF else 0x22000000), content, mask)

    /** Sets `a`'s content to an edge-to-edge page (bars in the page colour) and returns the column to fill. */
    @Suppress("DEPRECATION")
    fun screen(a: Activity): LinearLayout {
        val w = a.window
        w.setBackgroundDrawable(ColorDrawable(c.bg))
        w.statusBarColor = c.bg
        w.navigationBarColor = c.bg
        var flags = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
            View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
        if (!dark) flags = flags or View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR
        w.decorView.systemUiVisibility = flags
        val col = LinearLayout(ctx).apply { orientation = LinearLayout.VERTICAL }
        val root = ScrollView(ctx).apply {
            setBackgroundColor(c.bg)
            isFillViewport = true
            addView(col)
        }
        root.setOnApplyWindowInsetsListener { _, ins ->
            val (l, t, r, b) = if (Build.VERSION.SDK_INT >= 30) {
                val i = ins.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
                listOf(i.left, i.top, i.right, i.bottom)
            } else {
                listOf(ins.systemWindowInsetLeft, ins.systemWindowInsetTop, ins.systemWindowInsetRight, ins.systemWindowInsetBottom)
            }
            // The top inset pads the ScrollView itself, so content clips under the status bar; the bottom scrolls under the nav bar.
            root.setPadding(l, t, r, 0)
            col.setPadding(dp(Space.gutter), dp(Space.sm), dp(Space.gutter), b + dp(Space.xxxl))
            ins
        }
        a.setContentView(root)
        return col
    }

    /** The large bold screen title. */
    fun largeTitle(s: String) = text(s, TextSize.largeTitle, TextWeight.largeTitle, line = LineHeight.largeTitle).apply {
        layoutParams = lp(top = Space.xxl, bottom = Space.sm)
    }

    /** A footnote-sized secondary header over a card, inset to the row text. */
    fun sectionHeader(s: String) = text(s.uppercase(), TextSize.footnote, TextWeight.footnote, c.textSecondary).apply {
        setPadding(dp(Space.lg), 0, dp(Space.lg), 0)
        layoutParams = lp(top = Space.xl, bottom = 6f)
    }

    /** Secondary footnote text (explanations under cards). */
    fun footnote(s: CharSequence) = text(s, TextSize.footnote, TextWeight.footnote, c.textSecondary, LineHeight.footnote).apply {
        setPadding(dp(Space.lg), 0, dp(Space.lg), 0)
        layoutParams = lp(top = Space.sm)
    }

    /** A dark-grey rounded card; `padded = false` for grouped rows that bring their own padding. */
    fun card(padded: Boolean = true) = LinearLayout(ctx).apply {
        orientation = LinearLayout.VERTICAL
        background = shape(c.card, LucinaTokens.Radius.card)
        outlineProvider = ViewOutlineProvider.BACKGROUND
        clipToOutline = true
        if (padded) dp(Space.cardPadding).let { setPadding(it, it, it, it) }
        layoutParams = lp()
    }

    /** A card of rows with inset hairline separators between them (the iOS inset grouped list). */
    fun group(vararg rows: View) = card(padded = false).apply {
        rows.forEachIndexed { i, row ->
            if (i > 0) addView(View(ctx).apply {
                setBackgroundColor(c.separator)
                layoutParams = lp(h = maxOf(1, dp(0.5f))).apply { leftMargin = dp(Space.lg) }
            })
            addView(row)
        }
    }

    /** A plain row container (horizontal, 44 dp minimum, row padding) for custom content. */
    fun row() = LinearLayout(ctx).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        minimumHeight = dp(Space.tap)
        setPadding(dp(Space.lg), dp(11f), dp(Space.lg), dp(11f))
        layoutParams = lp()
    }

    private fun clickable(v: View, onClick: () -> Unit) {
        v.background = ripple(null, ColorDrawable(c.card))
        v.isClickable = true
        v.setOnClickListener { onClick() }
    }

    /** Label left, status dot and secondary value right; with `onClick` it is tappable, `chevron` adds a ›. */
    fun valueRow(label: String, value: String = "", chevron: Boolean = false, onClick: (() -> Unit)? = null): ValueRow {
        val row = row()
        val l = text(label).apply { layoutParams = lp(w = 0, weight = 1f) }
        val d = View(ctx).apply {
            background = shape(c.textTertiary, 0f, oval = true)
            visibility = View.GONE
            layoutParams = lp(w = dp(8f), h = dp(8f)).apply { leftMargin = dp(Space.md); rightMargin = dp(Space.sm) }
        }
        val v = text(value, color = c.textSecondary).apply { gravity = Gravity.END; maxLines = 2 }
        row.addView(l); row.addView(d); row.addView(v)
        if (chevron) row.addView(text("›", TextSize.title3, color = c.textTertiary).apply { setPadding(dp(Space.sm), 0, 0, 0) })
        if (onClick != null) clickable(row, onClick)
        return ValueRow(row, l, d, v)
    }

    /** A tappable row with its label in the accent colour (the iOS Form button), value on the right. */
    fun actionRow(label: String, color: Int = c.accent, onClick: () -> Unit) =
        valueRow(label, onClick = onClick).apply { this.label.setTextColor(color) }

    /** A destructive text button row (sign out, delete). */
    fun destructiveButton(label: String, onClick: () -> Unit) = actionRow(label, c.negative, onClick)

    private fun pill(label: String, fill: Int, fg: Int, onClick: () -> Unit) = Button(ctx).apply {
        text = label
        isAllCaps = false
        setTextSize(TypedValue.COMPLEX_UNIT_SP, TextSize.headline)
        typeface = font(TextWeight.headline)
        setTextColor(fg)
        isSingleLine = true
        ellipsize = TextUtils.TruncateAt.END
        stateListAnimator = null
        minHeight = 0; minimumHeight = dp(Space.tap); minWidth = 0; minimumWidth = 0
        setPadding(dp(Space.xl), dp(13f), dp(Space.xl), dp(13f))
        background = ripple(shape(fill, LucinaTokens.Radius.pill))
        setOnClickListener { onClick() }
        layoutParams = lp()
    }

    /** A filled accent capsule with white text. */
    fun primaryButton(label: String, onClick: () -> Unit) = pill(label, c.accentFill, c.onAccent, onClick)

    /** A soft accent capsule with accent text. */
    fun secondaryButton(label: String, onClick: () -> Unit) = pill(label, c.accentSoft, c.accent, onClick)

    /** Views side by side in equal widths with the grid gap (a pill pair). */
    fun pair(a: View, b: View) = LinearLayout(ctx).apply {
        orientation = LinearLayout.HORIZONTAL
        addView(a, lp(w = 0, weight = 1f))
        addView(b, lp(w = 0, weight = 1f).apply { leftMargin = dp(Space.gridGap) })
        layoutParams = lp()
    }

    /** Label and an accent-tinted Switch; `onToggle` runs on user taps only (row or switch), not on `isChecked = …`. */
    fun toggleRow(label: String, onToggle: (Boolean) -> Unit): Pair<LinearLayout, Switch> {
        val row = row()
        val sw = Switch(ctx)
        val states = arrayOf(intArrayOf(android.R.attr.state_checked), intArrayOf())
        sw.thumbTintList = ColorStateList(states, intArrayOf(c.accent, if (dark) 0xFFE5E5EA.toInt() else 0xFFFFFFFF.toInt()))
        sw.trackTintList = ColorStateList(states, intArrayOf(c.accent, c.textTertiary))
        sw.setOnClickListener { onToggle(sw.isChecked) }
        row.addView(text(label).apply { layoutParams = lp(w = 0, weight = 1f) })
        row.addView(sw)
        clickable(row) { sw.toggle(); onToggle(sw.isChecked) }
        return row to sw
    }

    /** A small coloured status dot. */
    fun dot(color: Int, size: Float = 8f) = View(ctx).apply {
        background = shape(color, 0f, oval = true)
        layoutParams = lp(w = dp(size), h = dp(size))
    }

    /** A capsule status chip: `color` text on a faint fill of the same colour. */
    fun chip(s: String, color: Int) = text(s, TextSize.caption, TextWeight.caption, color).apply {
        background = shape(color and 0x00FFFFFF or 0x2E000000, LucinaTokens.Radius.pill)
        setPadding(dp(10f), dp(Space.xs), dp(10f), dp(Space.xs))
        layoutParams = lp(w = -2)
    }

    /** Re-colours a chip made by [chip]. */
    fun recolor(chip: TextView, s: String, color: Int) {
        chip.text = s
        chip.setTextColor(color)
        (chip.background as GradientDrawable).setColor(color and 0x00FFFFFF or 0x2E000000)
    }

    /**
     * Like [screen] but without the ScrollView: a full-height column inset from the system bars, with no side
     * padding, for screens whose content scrolls itself (a WebView). Children use weights to fill it.
     */
    @Suppress("DEPRECATION")
    fun frame(a: Activity): LinearLayout {
        val w = a.window
        w.setBackgroundDrawable(ColorDrawable(c.bg))
        w.statusBarColor = c.bg
        w.navigationBarColor = c.bg
        var flags = View.SYSTEM_UI_FLAG_LAYOUT_STABLE or View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN or
            View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
        if (!dark) flags = flags or View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR
        w.decorView.systemUiVisibility = flags
        val root = LinearLayout(ctx).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(c.bg)
        }
        root.setOnApplyWindowInsetsListener { _, ins ->
            if (Build.VERSION.SDK_INT >= 30) {
                val i = ins.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout() or WindowInsets.Type.ime())
                root.setPadding(i.left, i.top, i.right, i.bottom)
            } else {
                root.setPadding(ins.systemWindowInsetLeft, ins.systemWindowInsetTop, ins.systemWindowInsetRight, ins.systemWindowInsetBottom)
            }
            ins
        }
        a.setContentView(root)
        return root
    }

    /** A capsule button in any fill (e.g. `c.positive` for Call, `c.negative` for Hang up), white text by default. */
    fun filledButton(label: String, fill: Int, fg: Int = c.onAccent, onClick: () -> Unit) = pill(label, fill, fg, onClick)

    /** A capsule text field on the `field` fill (the iOS PillInputRow field). */
    fun pillInput(hint: String, inputType: Int = android.text.InputType.TYPE_CLASS_TEXT, size: Float = TextSize.body) =
        android.widget.EditText(ctx).apply {
            this.hint = hint
            this.inputType = inputType
            setTextSize(TypedValue.COMPLEX_UNIT_SP, size)
            setTextColor(c.text)
            setHintTextColor(c.textTertiary)
            isSingleLine = true
            background = shape(c.field, LucinaTokens.Radius.pill)
            setPadding(dp(Space.xl), dp(Space.md), dp(Space.xl), dp(Space.md))
            minimumHeight = dp(Space.tap)
            layoutParams = lp()
        }

    /**
     * A chat bubble: the owner's (`mine`) right-aligned in `accentFill` with white text, Lucina's left-aligned
     * on `card`; at most 80% of the screen wide.
     */
    fun bubble(s: CharSequence, mine: Boolean) = text(s, TextSize.body, TextWeight.body, if (mine) c.onAccent else c.text, LineHeight.body).apply {
        background = shape(if (mine) c.accentFill else c.card, LucinaTokens.Radius.card)
        setPadding(dp(14f), dp(10f), dp(14f), dp(10f))
        maxWidth = (dm.widthPixels * 0.8f).toInt()
        layoutParams = lp(w = -2, top = Space.xs, bottom = Space.xs).apply { gravity = if (mine) Gravity.END else Gravity.START }
    }

    /** A round button of `size` dp in `fill` around `content` (a glyph, a label…), with a ripple. */
    fun circle(content: View, size: Float, fill: Int = c.card, onClick: (() -> Unit)? = null) = android.widget.FrameLayout(ctx).apply {
        background = ripple(shape(fill, 0f, oval = true), shape(-1, 0f, oval = true))
        addView(content, android.widget.FrameLayout.LayoutParams(-2, -2, Gravity.CENTER))
        if (onClick != null) {
            isClickable = true
            setOnClickListener { onClick() }
        }
        layoutParams = lp(w = dp(size), h = dp(size))
    }

    /** `button` (e.g. a [circle]) over a centred caption label, as in the iOS in-call controls. */
    fun labeled(button: View, label: String) = LinearLayout(ctx).apply {
        orientation = LinearLayout.VERTICAL
        gravity = Gravity.CENTER_HORIZONTAL
        addView(button)
        addView(text(label, TextSize.footnote, TextWeight.footnote, c.textSecondary).apply {
            gravity = Gravity.CENTER
            layoutParams = lp(w = -2, top = Space.sm)
        })
        layoutParams = lp(w = -2)
    }

    /** A dial-pad key: a [circle] on `card` with the digit over its letters; `onLong` for long presses (0 → +). */
    fun keypadKey(digit: String, letters: String, size: Float = 76f, onLong: (() -> Unit)? = null, onClick: () -> Unit): View {
        val face = LinearLayout(ctx).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            addView(text(digit, TextSize.title, 500, c.text).apply { gravity = Gravity.CENTER; includeFontPadding = false })
            addView(text(letters.ifEmpty { " " }, TextSize.caption2, TextWeight.caption, c.textSecondary).apply {
                gravity = Gravity.CENTER
                letterSpacing = 0.15f
            })
        }
        return circle(face, size, c.card, onClick).apply {
            if (onLong != null) setOnLongClickListener { onLong(); true }
        }
    }

    /** Views spread evenly across the width, each centred in an equal share (a row of round buttons). */
    fun spread(vararg views: View) = LinearLayout(ctx).apply {
        orientation = LinearLayout.HORIZONTAL
        for (v in views) addView(android.widget.FrameLayout(ctx).apply {
            val own = v.layoutParams // keep a fixed size (a circle), but not a match-parent width
            val w = if (own != null && own.width > 0) own.width else -2
            val h = if (own != null && own.height > 0) own.height else -2
            addView(v, android.widget.FrameLayout.LayoutParams(w, h, Gravity.CENTER))
        }, lp(w = 0, weight = 1f))
        layoutParams = lp()
    }
}
