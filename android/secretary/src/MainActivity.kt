package tech.getsecretary.secretary

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.graphics.Color
import android.os.Bundle
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.LinearLayout
import org.json.JSONObject

/** Secretary in a WebView, confined to getsecretary.tech by Rust's router, plus the Calls screen. */
class MainActivity : Activity() {
    private lateinit var web: WebView

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        val root = LinearLayout(this)
        root.orientation = LinearLayout.VERTICAL
        root.setBackgroundColor(Color.rgb(10, 14, 32))
        setContentView(root)
        val nav = LinearLayout(this)
        root.addView(nav)
        for (name in arrayOf("Lucina", "My desk", "Calls")) {
            val button = Button(this)
            button.text = name
            button.isAllCaps = false
            nav.addView(button, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            button.setOnClickListener {
                when (name) {
                    "Calls" -> startActivity(Intent(this, CallsActivity::class.java))
                    "My desk" -> web.loadUrl(Calls.SITE + "/desk")
                    else -> web.loadUrl(Calls.SITE + "/")
                }
            }
        }
        web = WebView(this)
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
