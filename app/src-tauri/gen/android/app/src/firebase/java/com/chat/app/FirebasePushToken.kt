package com.chat.app

import android.content.Context
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging

internal object FirebasePushToken {
  const val supported = true
  @Volatile private var latest: String? = null

  fun refreshed(token: String) {
    latest = token
  }

  fun read(context: Context, done: (String?, String?) -> Unit) {
    val project = FirebaseApp.getApps(context).find { it.name == FirebaseApp.DEFAULT_APP_NAME }?.options?.projectId
    if (project == null) {
      done(null, null)
      return
    }
    val cached = latest
    if (cached != null) {
      done(project, cached)
      return
    }
    FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
      done(project, latest ?: if (task.isSuccessful) task.result else null)
    }
  }
}
