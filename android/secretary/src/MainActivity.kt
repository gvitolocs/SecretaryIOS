package tech.getsecretary.secretary

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.os.Bundle
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.LinearLayout
import org.json.JSONObject
import tech.getsecretary.design.LucinaUi

/** Secretary in a WebView, confined to getsecretary.tech by Rust's router, plus the Calls screen. */
class MainActivity : Activity() {
    private lateinit var web: WebView

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        val ui = LucinaUi(this)
        val root = ui.frame(this)
        val header = LinearLayout(this)
        header.orientation = LinearLayout.VERTICAL
        header.setPadding(ui.dp(16f), 0, ui.dp(16f), ui.dp(12f))
        root.addView(header)
        header.addView(ui.largeTitle("Secretary").apply { layoutParams = ui.lp(top = 12f, bottom = 12f) })
        val nav = LinearLayout(this)
        header.addView(nav)
        for ((i, name) in arrayOf("Lucina", "My desk", "Calls").withIndex()) {
            val button = ui.secondaryButton(name) {
                when (name) {
                    "Calls" -> startActivity(Intent(this, CallsActivity::class.java))
                    "My desk" -> web.loadUrl(Calls.SITE + "/desk")
                    else -> web.loadUrl(Calls.SITE + "/")
                }
            }
            nav.addView(button, ui.lp(w = 0, weight = 1f).apply { if (i > 0) leftMargin = ui.dp(8f) })
        }
        web = WebView(this)
        web.setBackgroundColor(ui.c.bg)
        root.addView(web, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
        val s = web.settings
        s.javaScriptEnabled = true
        s.domStorageEnabled = true
        s.allowFileAccess = false
        s.allowContentAccess = false
        s.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
        CookieManager.getInstance().setAcceptCookie(true)
        CookieManager.getInstance().setAcceptThirdPartyCookies(web, false)
        web.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                val uri = request.url
                when (route(uri.toString())) {
                    "web" -> return false
                    "calls" -> startActivity(Intent(this@MainActivity, CallsActivity::class.java).setData(uri))
                    "external" -> try {
                        startActivity(Intent(Intent.ACTION_VIEW, uri))
                    } catch (e: ActivityNotFoundException) {
                    }
                }
                return true
            }
        }
        val saved = b?.getString("url")
        web.loadUrl(if (saved != null && route(saved) == "web") saved else Calls.SITE + "/")
    }

    /** `web`, `calls`, `external` or `block`. */
    private fun route(url: String): String =
        try {
            Calls.ui("route", JSONObject().put("url", url)).optString("action", "block")
        } catch (e: Exception) {
            "block"
        }

    override fun onSaveInstanceState(b: Bundle) {
        super.onSaveInstanceState(b)
        b.putString("url", web.url)
    }

    @Deprecated("Activity back handling; there is no AndroidX here.")
    @Suppress("DEPRECATION")
    override fun onBackPressed() {
        if (web.canGoBack()) web.goBack() else super.onBackPressed()
    }
}
