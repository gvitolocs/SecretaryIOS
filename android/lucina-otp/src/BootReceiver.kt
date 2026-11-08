package tech.getsecretary.lucinaotp

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import org.json.JSONObject

/** Jobs do not survive a reboot; re-arm the retry job if codes are still queued. */
class BootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        val app = context.applicationContext
        val pending = goAsync()
        Thread({
            try {
                Core.ensure(app)
                if (JSONObject(Native.status()).optInt("pending") > 0) RetryJobService.schedule(app)
            } catch (t: Throwable) {
                Log.w(TAG, "boot result=error")
            } finally {
                pending.finish()
            }
        }, "lucina-otp-boot").start()
    }
}
