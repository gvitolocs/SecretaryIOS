package tech.getsecretary.secretary

import android.Manifest
import android.app.job.JobInfo
import android.app.job.JobParameters
import android.app.job.JobScheduler
import android.app.job.JobService
import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import android.os.PersistableBundle
import java.io.File
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONObject

/** Android owns the durable network job; Rust owns the upload and retry classification. */
class ShareJobService : JobService() {
    private val stopped = ConcurrentHashMap<Int, AtomicBoolean>()

    override fun onStartJob(params: JobParameters): Boolean {
        val path = params.extras.getString("manifest") ?: return false
        // Read CookieManager on the main thread; no token or cookie is persisted with a job.
        val cookie = Calls.cookie()
        val stop = AtomicBoolean(false)
        stopped[params.jobId] = stop
        Thread {
            var retry = true
            try {
                val result = JSONObject(Native.shareUpload(filesDir.absolutePath, cookie, path))
                retry = result.optBoolean("retry", true)
                val status = result.optInt("status")
                if (result.optBoolean("ok") || (!retry && status in 400..499)) {
                    val folder = File(path).canonicalFile.parentFile
                    val root = File(cacheDir, "shares").canonicalFile
                    if (folder != null && folder.parentFile == root) folder.deleteRecursively()
                }
            } catch (e: Exception) {
                // An interrupted job keeps its staged files and is retried by Android.
            } finally {
                if (stopped[params.jobId] === stop) stopped.remove(params.jobId)
                if (!stop.get()) jobFinished(params, retry)
            }
        }.start()
        return true
    }

    override fun onStopJob(params: JobParameters): Boolean {
        stopped.remove(params.jobId)?.set(true)
        return true
    }

    companion object {
        fun schedule(c: Context, manifest: File) {
            val scheduler = c.getSystemService(JobScheduler::class.java)
            val used = scheduler.allPendingJobs.map { it.id }.toSet()
            var id = manifest.parentFile!!.name.hashCode() and 0x7fffffff
            while (id in used) id = (id + 1) and 0x7fffffff
            val extras = PersistableBundle().apply { putString("manifest", manifest.absolutePath) }
            val job = JobInfo.Builder(id, ComponentName(c, ShareJobService::class.java))
                .setExtras(extras)
                .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
                .setBackoffCriteria(30_000, JobInfo.BACKOFF_POLICY_EXPONENTIAL)
            if (c.checkSelfPermission(Manifest.permission.RECEIVE_BOOT_COMPLETED) == PackageManager.PERMISSION_GRANTED) {
                job.setPersisted(true)
            }
            check(scheduler.schedule(job.build()) == JobScheduler.RESULT_SUCCESS) { "Could not schedule the share. Try again." }
        }
    }
}
