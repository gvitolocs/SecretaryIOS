package tech.getsecretary.lucinaotp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Handler
import android.os.Looper
import android.provider.Telephony
import android.util.Log
import java.util.concurrent.atomic.AtomicBoolean

class SmsReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Telephony.Sms.Intents.SMS_RECEIVED_ACTION) return
        val receivedAt = System.currentTimeMillis()
        val parts = Telephony.Sms.Intents.getMessagesFromIntent(intent) ?: return
        var sender: String? = null
        val body = StringBuilder()
        for (sms in parts) {
            if (sms == null) continue
            if (sender == null) sender = sms.originatingAddress
            body.append(sms.messageBody ?: "")
        }
        if (body.isEmpty() || body.length > 2048) return
        val from = sender ?: ""
        val text = body.toString()
        val app = context.applicationContext
        val pending = goAsync()
        val done = AtomicBoolean(false)
        val finish = Runnable { if (done.compareAndSet(false, true)) pending.finish() }
        Handler(Looper.getMainLooper()).postDelayed(finish, 9_000)
        Thread({
            try {
                Core.ensure(app)
                if (Core.log("sms", Native.onSms(from, text, receivedAt)) == "queued") RetryJobService.schedule(app)
            } catch (t: Throwable) {
                Log.w(TAG, "sms result=error ms=${System.currentTimeMillis() - receivedAt}")
            } finally {
                finish.run()
            }
        }, "lucina-otp-sms").start()
    }
}
