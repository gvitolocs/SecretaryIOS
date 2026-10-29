package tech.getsecretary.lucinaotp

import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.util.Log
import org.json.JSONObject

/** Flushes queued codes once a network is up; Rust drops the ones that expired meanwhile. */
class RetryJobService : JobService() {
    override fun onStartJob(params: JobParameters): Boolean {
        val app = applicationContext
        Thread({
            var retry = false
            try {
                Core.ensure(app)
                val r = JSONObject(Native.flush())
                retry = r.optBoolean("retry")
                Log.i(TAG, "flush result=sent:${r.optInt("sent")},dropped:${r.optInt("dropped")},pending:${r.optInt("pending")}")
            } catch (t: Throwable) {
                Log.w(TAG, "flush result=error")
            } finally {
                jobFinished(params, retry)
            }
        }, "lucina-otp-flush").start()
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean = true

    companion object {
        private const val JOB_ID = 0x4c4f // "LO"

        fun schedule(context: Context) {
            val js = context.getSystemService(JobScheduler::class.java) ?: return
            if (js.getPendingJob(JOB_ID) != null) return
            js.schedule(
                JobInfo.Builder(JOB_ID, ComponentName(context, RetryJobService::class.java))
                    .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
                    .setBackoffCriteria(10_000, JobInfo.BACKOFF_POLICY_LINEAR)
                    .build(),
            )
        }
    }
}
