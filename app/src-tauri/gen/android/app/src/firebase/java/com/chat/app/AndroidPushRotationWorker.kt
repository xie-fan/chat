package com.chat.app

import android.content.Context
import androidx.work.Worker
import androidx.work.WorkerParameters
import org.json.JSONObject
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

class AndroidPushRotationWorker(context: Context, params: WorkerParameters) : Worker(context, params) {
  @Volatile private var connection: HttpURLConnection? = null

  override fun doWork(): Result {
    val rotation = AndroidPushBinding.rotation(applicationContext) ?: return Result.success()
    val endpoint = URL("${rotation.server}/v1/push/android/rotate")
    val request = endpoint.openConnection() as HttpURLConnection
    connection = request
    return try {
      if (isStopped || AndroidPushBinding.rotation(applicationContext) != rotation) return Result.success()
      request.instanceFollowRedirects = false
      request.connectTimeout = 10_000
      request.readTimeout = 10_000
      request.requestMethod = "POST"
      request.setRequestProperty("Content-Type", "application/json")
      request.doOutput = true
      request.outputStream.use { it.write(JSONObject(rotation.registration()).toString().toByteArray(Charsets.UTF_8)) }
      when (request.responseCode) {
        in 200..299 -> Result.success()
        401 -> if (runAttemptCount < 4) Result.retry() else Result.failure()
        408, 429, in 500..599 -> Result.retry()
        else -> Result.failure()
      }
    } catch (_: IOException) {
      Result.retry()
    } finally {
      request.disconnect()
      connection = null
    }
  }

  override fun onStopped() {
    connection?.disconnect()
  }
}
