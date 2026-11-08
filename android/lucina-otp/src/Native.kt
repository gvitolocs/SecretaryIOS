package tech.getsecretary.lucinaotp

/**
 * JNI surface of liblucina_android.so (crates/lucina-android). Every call returns a JSON string,
 * is synchronous and must not run on the main thread. See docs/lucina-android.md.
 */
object Native {
    init {
        System.loadLibrary("lucina_android")
    }

    @JvmStatic external fun init(filesDir: String, dataKey: ByteArray, legacyToken: String?): String

    @JvmStatic external fun onSms(sender: String, body: String, receivedAtMs: Long): String

    @JvmStatic external fun onNotification(pkg: String, title: String, text: String, postedAtMs: Long): String

    @JvmStatic external fun flush(): String

    @JvmStatic external fun status(): String

    @JvmStatic external fun configure(json: String): String

    @JvmStatic external fun selfTest(network: Boolean): String
}
