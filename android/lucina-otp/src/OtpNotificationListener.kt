package tech.getsecretary.lucinaotp

import android.app.Notification
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import android.util.Log
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

class OtpNotificationListener : NotificationListenerService() {
    private val executor: ExecutorService = Executors.newSingleThreadExecutor()

    override fun onNotificationPosted(sbn: StatusBarNotification) {
        val n = sbn.notification ?: return
        if (sbn.isOngoing || (n.flags and Notification.FLAG_GROUP_SUMMARY) != 0) return
        val pkg = sbn.packageName ?: return
        if (pkg == packageName) return
        val extras = n.extras ?: return
        val title = extras.getCharSequence(Notification.EXTRA_TITLE)?.toString() ?: ""
        val text = (extras.getCharSequence(Notification.EXTRA_BIG_TEXT)
            ?: extras.getCharSequence(Notification.EXTRA_TEXT))?.toString() ?: return
        if (text.isEmpty() || text.length > 2048) return
        val posted = sbn.postTime
        val app = applicationContext
        try {
            executor.execute {
                try {
                    Core.ensure(app)
                    val json = Native.onNotification(pkg, title, text, posted)
                    // Every notification reaches the core; log only the ones that mattered.
                    val result = org.json.JSONObject(json).optString("result")
                    if (result != "disabled" && result != "not_otp") {
                        if (Core.log("notification", json) == "queued") RetryJobService.schedule(app)
                    }
                } catch (t: Throwable) {
                    Log.w(TAG, "notification result=error")
                }
            }
        } catch (_: java.util.concurrent.RejectedExecutionException) {
        }
    }

    override fun onDestroy() {
        executor.shutdown()
        super.onDestroy()
    }
}
