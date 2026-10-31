package tech.getsecretary.secretary

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ContentValues
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.webkit.CookieManager
import android.webkit.GeolocationPermissions
import android.webkit.MimeTypeMap
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
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
    private var chooser: ValueCallback<Array<Uri>>? = null
    private var captures: List<Uri> = emptyList()
    private val locations = mutableListOf<Pair<String, GeolocationPermissions.Callback>>()

    @Suppress("DEPRECATION")
    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        ShareShortcut.publish(this)
        val ui = LucinaUi(this)
        val root = ui.frame(this)
        val header = LinearLayout(this)
        header.orientation = LinearLayout.VERTICAL
        header.setPadding(ui.dp(16f), 0, ui.dp(16f), ui.dp(12f))
        root.addView(header)
        header.addView(ui.largeTitle("Secretary").apply { layoutParams = ui.lp(top = 12f, bottom = 12f) })
        val nav = LinearLayout(this)
        header.addView(nav)
        for ((i, name) in arrayOf("Lucina", "My desk", "Calls", "Callers").withIndex()) {
            val button = ui.secondaryButton(name) {
                when (name) {
                    "Calls" -> startActivity(Intent(this, CallsActivity::class.java))
                    "Callers" -> startActivity(Intent(this, CallerScreenActivity::class.java))
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
        s.setGeolocationEnabled(true)
        // The web shows Android-only cards (Phone) when it sees this; matched by /\bSecretaryAndroid\//.
        val version = packageManager.getPackageInfo(packageName, 0).versionName ?: "0"
        s.userAgentString = s.userAgentString + " SecretaryAndroid/" + version
        CookieManager.getInstance().setAcceptCookie(true)
        CookieManager.getInstance().setAcceptThirdPartyCookies(web, false)
        web.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                val uri = request.url
                when (route(uri.toString())) {
                    "web" -> return false
                    // tel: carries a number; secretary://calls (the web Phone card) opens the screen empty.
                    "calls" -> startActivity(
                        Intent(this@MainActivity, CallsActivity::class.java).apply { if (uri.scheme == "tel") data = uri }
                    )
                    "external" -> try {
                        startActivity(Intent(Intent.ACTION_VIEW, uri))
                    } catch (e: ActivityNotFoundException) {
                    }
                }
                return true
            }
        }
        web.webChromeClient = object : WebChromeClient() {
            override fun onShowFileChooser(
                view: WebView?,
                callback: ValueCallback<Array<Uri>>,
                params: FileChooserParams,
            ): Boolean = attachments(callback, params)

            override fun onGeolocationPermissionsShowPrompt(origin: String, callback: GeolocationPermissions.Callback) {
                if (!trusted(origin)) {
                    callback.invoke(origin, false, false)
                } else if (locationGranted()) {
                    callback.invoke(origin, true, false)
                } else {
                    locations.add(origin to callback)
                    if (locations.size == 1) {
                        requestPermissions(
                            arrayOf(Manifest.permission.ACCESS_FINE_LOCATION, Manifest.permission.ACCESS_COARSE_LOCATION),
                            REQUEST_LOCATION,
                        )
                    }
                }
            }
        }
        val saved = b?.getString("url")
        web.loadUrl(if (saved != null && route(saved) == "web") saved else Calls.SITE + "/")
    }

    /**
     * Android's file picker and camera for `<input type=file>`: accept types, multiple selection and
     * `capture`. Camera output goes to a MediaStore content URI, so no FileProvider is needed.
     */
    @Suppress("DEPRECATION")
    private fun attachments(callback: ValueCallback<Array<Uri>>, params: WebChromeClient.FileChooserParams): Boolean {
        // Keep the open chooser and its callback coherent; a second request is declined until it returns.
        if (chooser != null) {
            callback.onReceiveValue(null)
            return true
        }
        chooser = callback
        captures = emptyList()
        val multiple = params.mode == WebChromeClient.FileChooserParams.MODE_OPEN_MULTIPLE
        val types = mimeTypes(params.acceptTypes)
        val all = types.isEmpty() || types.contains("*/*")
        fun allows(kind: String) = all || types.any { it == "$kind/*" || it.startsWith("$kind/") }
        val picker = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = if (types.size == 1) types[0] else "*/*"
            if (!all && types.size > 1) putExtra(Intent.EXTRA_MIME_TYPES, types.toTypedArray())
            if (multiple) putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true)
        }
        val camera = ArrayList<Intent>()
        if (params.isCaptureEnabled) {
            if (allows("image")) capture(false)?.let { camera.add(it) }
            if (allows("video")) capture(true)?.let { camera.add(it) }
        }
        captures = camera.mapNotNull { it.getParcelableExtra<Uri>(MediaStore.EXTRA_OUTPUT) }
        camera.forEach { grant(it) }
        val target = if (camera.isEmpty()) picker else Intent.createChooser(picker, "Attach").apply {
            putExtra(Intent.EXTRA_INITIAL_INTENTS, camera.toTypedArray())
        }
        return try {
            startActivityForResult(target, REQUEST_ATTACH)
            true
        } catch (e: Exception) {
            discardCaptures()
            chooser = null
            callback.onReceiveValue(null)
            true
        }
    }

    /** The file types the page accepts, as MIME types (everything when it accepts anything). */
    private fun mimeTypes(raw: Array<String>): List<String> {
        val out = linkedSetOf<String>()
        for (value in raw) {
            for (part in value.split(',')) {
                val type = part.trim().lowercase()
                when {
                    type == "*/*" -> return listOf("*/*")
                    type.contains('/') -> out.add(type)
                    type.startsWith('.') ->
                        MimeTypeMap.getSingleton().getMimeTypeFromExtension(type.substring(1))?.let { out.add(it) }
                }
            }
        }
        return out.toList()
    }

    /** A capture intent whose output Uri is a pending MediaStore entry. */
    private fun capture(video: Boolean): Intent? {
        val values = ContentValues().apply {
            val ext = if (video) ".mp4" else ".jpg"
            put(MediaStore.MediaColumns.DISPLAY_NAME, "Lucina-" + System.currentTimeMillis() + ext)
            put(MediaStore.MediaColumns.MIME_TYPE, if (video) "video/mp4" else "image/jpeg")
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val collection = if (video) MediaStore.Video.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
            else MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL_PRIMARY)
        val uri = try {
            contentResolver.insert(collection, values)
        } catch (e: Exception) {
            null
        } ?: return null
        val intent = Intent(if (video) MediaStore.ACTION_VIDEO_CAPTURE else MediaStore.ACTION_IMAGE_CAPTURE).apply {
            putExtra(MediaStore.EXTRA_OUTPUT, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
            clipData = ClipData.newRawUri("capture", uri)
        }
        if (video) intent.putExtra(MediaStore.EXTRA_VIDEO_QUALITY, 1)
        if (packageManager.queryIntentActivities(intent, PackageManager.MATCH_DEFAULT_ONLY).isEmpty()) {
            delete(uri)
            return null
        }
        return intent
    }

    /** Explicit camera grants survive the chooser's handling of EXTRA_INITIAL_INTENTS. */
    @Suppress("DEPRECATION")
    private fun grant(intent: Intent) {
        val uri = intent.getParcelableExtra<Uri>(MediaStore.EXTRA_OUTPUT) ?: return
        val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        for (target in packageManager.queryIntentActivities(intent, PackageManager.MATCH_DEFAULT_ONLY)) {
            try {
                grantUriPermission(target.activityInfo.packageName, uri, flags)
            } catch (e: Exception) {
            }
        }
    }

    @Deprecated("Activity result API; there is no AndroidX here.")
    @Suppress("DEPRECATION")
    override fun onActivityResult(request: Int, result: Int, data: Intent?) {
        super.onActivityResult(request, result, data)
        if (request != REQUEST_ATTACH) return
        val callback = chooser ?: return
        chooser = null
        val pending = captures
        captures = emptyList()
        if (result != RESULT_OK) {
            pending.forEach { delete(it) }
            callback.onReceiveValue(null)
            return
        }
        val uris = linkedSetOf<Uri>()
        data?.data?.let { uris.add(it) }
        data?.clipData?.let { clip ->
            for (i in 0 until clip.itemCount) clip.getItemAt(i).uri?.let { uris.add(it) }
        }
        // With two camera items the result does not say which one was chosen; the written file has bytes.
        val shot = pending.firstOrNull { size(it) > 0 }
        pending.forEach { if (it != shot) delete(it) }
        if (shot != null) {
            publish(shot)
            uris.add(shot)
        }
        callback.onReceiveValue(if (uris.isEmpty()) null else uris.toTypedArray())
    }

    private fun size(uri: Uri): Long {
        var found = -1L
        try {
            contentResolver.query(uri, arrayOf(OpenableColumns.SIZE), null, null, null)?.use { c ->
                if (c.moveToFirst() && !c.isNull(0)) found = c.getLong(0)
            }
        } catch (e: Exception) {
        }
        if (found > 0) return found
        // A pending row can still report 0 until the camera closes its descriptor; stat the file itself.
        return try {
            contentResolver.openAssetFileDescriptor(uri, "r")?.use { it.length } ?: found
        } catch (e: Exception) {
            found
        }
    }

    private fun publish(uri: Uri) {
        try {
            val done = ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }
            contentResolver.update(uri, done, null, null)
        } catch (e: Exception) {
        }
        ungrant(uri)
    }

    private fun delete(uri: Uri) {
        try {
            contentResolver.delete(uri, null, null)
        } catch (e: Exception) {
        }
        ungrant(uri)
    }

    private fun ungrant(uri: Uri) {
        try {
            revokeUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        } catch (e: Exception) {
        }
    }

    private fun discardCaptures() {
        captures.forEach { delete(it) }
        captures = emptyList()
    }

    /** Only Lucina's own HTTPS origin may use the phone's location. */
    private fun trusted(origin: String): Boolean {
        val uri = Uri.parse(origin)
        return uri.scheme == "https" && uri.host == "getsecretary.tech"
    }

    private fun locationGranted(): Boolean =
        checkSelfPermission(Manifest.permission.ACCESS_FINE_LOCATION) == PackageManager.PERMISSION_GRANTED ||
            checkSelfPermission(Manifest.permission.ACCESS_COARSE_LOCATION) == PackageManager.PERMISSION_GRANTED

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        if (r != REQUEST_LOCATION) return
        val granted = locationGranted()
        val waiting = locations.toList()
        locations.clear()
        // Not retained: a reload asks again.
        for ((origin, callback) in waiting) callback.invoke(origin, granted, false)
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

    private companion object {
        const val REQUEST_ATTACH = 10
        const val REQUEST_LOCATION = 11
    }
}
