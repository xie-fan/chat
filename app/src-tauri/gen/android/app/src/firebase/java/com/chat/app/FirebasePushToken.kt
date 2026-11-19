package com.chat.app

import android.content.Context
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging

internal object FirebasePushToken {
  const val supported = true
  private const val WORK_NAME = "chat-fcm-token-rotation"

  fun refreshed(context: Context, token: String) {
    val project = project(context) ?: return
    if (AndroidPushBinding.record(context, null, token, project) == null) return
    val request = OneTimeWorkRequestBuilder<AndroidPushRotationWorker>()
      .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
      .build()
    WorkManager.getInstance(context).enqueueUniqueWork(WORK_NAME, ExistingWorkPolicy.REPLACE, request)
  }

  fun cancel(context: Context) {
    WorkManager.getInstance(context).cancelUniqueWork(WORK_NAME)
  }

  private fun project(context: Context) =
    FirebaseApp.getApps(context).find { it.name == FirebaseApp.DEFAULT_APP_NAME }?.options?.projectId

  fun read(context: Context, binding: String, done: (AndroidPushRotation?) -> Unit) {
    val project = project(context)
    if (project == null) {
      done(null)
      return
    }
    val generation = AndroidPushBinding.rotation(context)?.generation ?: 0
    FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
      done(if (task.isSuccessful) AndroidPushBinding.record(context, binding, task.result, project, generation) else null)
    }
  }
}
