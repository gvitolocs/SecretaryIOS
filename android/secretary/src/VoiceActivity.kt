package tech.getsecretary.secretary

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.speech.tts.TextToSpeech
import android.speech.tts.UtteranceProgressListener
import android.telecom.TelecomManager
import android.widget.TextView
import java.util.Locale
import org.json.JSONObject

/**
 * A voice conversation with Lucina over the internet: SpeechRecognizer -> Rust `voiceTurn`
 * (journal + HTTPS) -> TextToSpeech. Sensitive replies are shown, never spoken.
 */
class VoiceActivity : Ui(), RecognitionListener {
    private var recognizer: SpeechRecognizer? = null
    private var tts: TextToSpeech? = null
    private lateinit var status: TextView
    private lateinit var dialogue: TextView

    /** The journal record, once Rust has created it. */
    private var id: String? = null
    private var ended = false
    private var ready = false

    override fun onCreate(b: Bundle?) {
        super.onCreate(b)
        screen("Call Lucina · Internet")
        status = text("Starting…", 18)
        dialogue = text("", 16)
        button("Hang up") { hangup() }
        text(
            "This is a voice conversation with Lucina over your internet connection. Speech recognition uses the " +
                "phone’s configured speech service. Your transcript appears in Calls afterwards. This does not record a SIM call.",
            13,
        )
        if (checkSelfPermission(Manifest.permission.READ_PHONE_STATE) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO, Manifest.permission.READ_PHONE_STATE), REQUEST_MIC)
            return
        }
        if (inCall()) {
            status.text = "Finish the phone call before starting a conversation with Lucina."
            return
        }
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), REQUEST_MIC)
            return
        }
        start()
    }

    private fun inCall(): Boolean = getSystemService(TelecomManager::class.java).isInCall

    private fun start() {
        if (checkSelfPermission(Manifest.permission.READ_PHONE_STATE) != PackageManager.PERMISSION_GRANTED || inCall()) {
            status.text = "Phone state access is required, and a SIM call must finish first."
            return
        }
        if (!SpeechRecognizer.isRecognitionAvailable(this)) {
            status.text = "Speech recognition is unavailable. Use Lucina’s text chat."
            return
        }
        val app = applicationContext
        Calls.io.execute {
            val started = try {
                Calls.begin(app, "internet", "Lucina")
            } catch (e: Exception) {
                runOnUiThread { status.text = e.message }
                null
            }
            if (started != null) {
                runOnUiThread {
                    if (ended) {
                        Calls.endLater(app, started, "ended")
                    } else {
                        id = started
                        converse()
                    }
                }
            }
        }
    }

    private fun converse() {
        val r = SpeechRecognizer.createSpeechRecognizer(this)
        r.setRecognitionListener(this)
        recognizer = r
        tts = TextToSpeech(this) { code ->
            ready = code == TextToSpeech.SUCCESS
            val t = tts
            if (ready && t != null) {
                t.language = Locale.getDefault()
                t.setOnUtteranceProgressListener(object : UtteranceProgressListener() {
                    override fun onStart(u: String?) {}

                    override fun onDone(u: String?) {
                        runOnUiThread { listen() }
                    }

                    @Suppress("OVERRIDE_DEPRECATION") // Abstract in the framework; the newer overload calls it.
                    override fun onError(u: String?) {
                        runOnUiThread { status.text = "Voice playback failed. Tap Listen to continue." }
                    }
                })
            }
            listen()
        }
        button("Listen") { listen() }
    }

    private fun listen() {
        val r = recognizer ?: return
        if (ended) return
        status.text = "Listening…"
        val i = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
        i.putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
        i.putExtra(RecognizerIntent.EXTRA_LANGUAGE, Locale.getDefault().toLanguageTag())
        i.putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, false)
        r.startListening(i)
    }

    private fun line(role: String, text: String) {
        dialogue.append((if (role == "user") "You: " else "Lucina: ") + text + "\n\n")
    }

    override fun onResults(b: Bundle?) {
        val heard = b?.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()
        val callId = id
        if (ended || heard == null || heard.isBlank() || callId == null) return
        line("user", heard)
        status.text = "Lucina is thinking…"
        val files = filesDir.absolutePath
        Thread({
            try {
                val args = JSONObject().put("id", callId).put("text", heard).toString()
                val reply = unwrap(Native.voiceTurn(files, Calls.cookie(), args))
                val answer = reply.optString("reply", "No reply was returned.")
                runOnUiThread {
                    if (!ended) {
                        line("lucina", answer)
                        status.text = "Lucina"
                        val t = tts
                        if (ready && t != null && reply.optBoolean("speak")) {
                            t.speak(answer, TextToSpeech.QUEUE_FLUSH, null, "reply")
                        } else {
                            status.text = answer + "\nTap Listen to continue."
                        }
                    }
                }
            } catch (e: Exception) {
                runOnUiThread { if (!ended) status.text = e.message + "\nTap Listen to retry." }
            }
        }, "lucina-voice").start()
    }

    /** Stop listening and speaking and close the journal record, once. */
    private fun stop() {
        if (ended) return
        ended = true
        id?.let { Calls.endLater(this, it, "ended") }
        recognizer?.cancel()
        tts?.stop()
    }

    private fun hangup() {
        if (ended) return
        stop()
        startActivity(Intent(this, TranscriptActivity::class.java))
        finish()
    }

    override fun onRequestPermissionsResult(r: Int, p: Array<String>, g: IntArray) {
        super.onRequestPermissionsResult(r, p, g)
        if (r == REQUEST_MIC && g.isNotEmpty() && g[0] == PackageManager.PERMISSION_GRANTED) {
            start()
        } else {
            status.text = "Microphone permission is required. Text chat remains available."
        }
    }

    override fun onError(error: Int) {
        if (!ended) status.text = "Listening stopped ($error). Tap Listen to continue."
    }

    override fun onReadyForSpeech(b: Bundle?) {}

    override fun onBeginningOfSpeech() {}

    override fun onRmsChanged(r: Float) {}

    override fun onBufferReceived(b: ByteArray?) {}

    override fun onEndOfSpeech() {
        status.text = "Recognizing…"
    }

    override fun onPartialResults(b: Bundle?) {}

    override fun onEvent(t: Int, b: Bundle?) {}

    override fun onStop() {
        super.onStop()
        if (!isChangingConfigurations) stop()
    }

    override fun onDestroy() {
        recognizer?.destroy()
        tts?.shutdown()
        super.onDestroy()
    }

    private companion object {
        const val REQUEST_MIC = 3
    }
}
