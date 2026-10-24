package tech.getsecretary.secretary

import android.graphics.Canvas
import android.graphics.ColorFilter
import android.graphics.Paint
import android.graphics.Path
import android.graphics.PixelFormat
import android.graphics.RectF
import android.graphics.drawable.Drawable

/** Line icons drawn in code (no resources): stroke glyphs on a 24-unit grid, scaled to the bounds. */
class Glyph(private val kind: Kind, color: Int, private val sizePx: Int) : Drawable() {
    enum class Kind { MIC, MIC_OFF, SPEAKER, EARPIECE, PAUSE, PLAY, BACKSPACE }

    private val stroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND
        strokeJoin = Paint.Join.ROUND
        this.color = color
    }
    private val fill = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.FILL
        this.color = color
    }

    override fun getIntrinsicWidth() = sizePx

    override fun getIntrinsicHeight() = sizePx

    override fun draw(canvas: Canvas) {
        val b = bounds
        val s = minOf(b.width(), b.height()) / 24f
        canvas.save()
        canvas.translate(b.left + (b.width() - 24 * s) / 2, b.top + (b.height() - 24 * s) / 2)
        canvas.scale(s, s)
        stroke.strokeWidth = 2f
        when (kind) {
            Kind.MIC, Kind.MIC_OFF -> {
                canvas.drawRoundRect(RectF(9f, 2.5f, 15f, 14.5f), 3f, 3f, fill)
                canvas.drawArc(RectF(5.5f, 5.5f, 18.5f, 17f), 0f, 180f, false, stroke)
                canvas.drawLine(12f, 17f, 12f, 21f, stroke)
                canvas.drawLine(8.5f, 21f, 15.5f, 21f, stroke)
                if (kind == Kind.MIC_OFF) canvas.drawLine(4f, 3f, 20f, 21f, stroke)
            }
            Kind.SPEAKER -> {
                canvas.drawPath(Path().apply {
                    moveTo(3f, 9f); lineTo(7f, 9f); lineTo(12f, 4.5f); lineTo(12f, 19.5f); lineTo(7f, 15f); lineTo(3f, 15f); close()
                }, fill)
                canvas.drawArc(RectF(10f, 8f, 18f, 16f), -50f, 100f, false, stroke)
                canvas.drawArc(RectF(9f, 4.5f, 22f, 19.5f), -50f, 100f, false, stroke)
            }
            Kind.EARPIECE -> {
                canvas.drawRoundRect(RectF(7f, 2.5f, 17f, 21.5f), 2.5f, 2.5f, stroke)
                canvas.drawLine(10.5f, 5.5f, 13.5f, 5.5f, stroke)
                canvas.drawCircle(12f, 18.5f, 1f, fill)
            }
            Kind.PAUSE -> {
                canvas.drawRoundRect(RectF(6.5f, 5f, 10f, 19f), 1f, 1f, fill)
                canvas.drawRoundRect(RectF(14f, 5f, 17.5f, 19f), 1f, 1f, fill)
            }
            Kind.PLAY -> canvas.drawPath(Path().apply { moveTo(7.5f, 4.5f); lineTo(19f, 12f); lineTo(7.5f, 19.5f); close() }, fill)
            Kind.BACKSPACE -> {
                canvas.drawPath(Path().apply {
                    moveTo(8f, 5f); lineTo(21f, 5f); lineTo(21f, 19f); lineTo(8f, 19f); lineTo(2.5f, 12f); close()
                }, stroke)
                canvas.drawLine(11.5f, 9f, 17.5f, 15f, stroke)
                canvas.drawLine(17.5f, 9f, 11.5f, 15f, stroke)
            }
        }
        canvas.restore()
    }

    override fun setAlpha(alpha: Int) {
        stroke.alpha = alpha
        fill.alpha = alpha
    }

    override fun setColorFilter(cf: ColorFilter?) {
        stroke.colorFilter = cf
        fill.colorFilter = cf
    }

    @Deprecated("Drawable API")
    override fun getOpacity() = PixelFormat.TRANSLUCENT
}
