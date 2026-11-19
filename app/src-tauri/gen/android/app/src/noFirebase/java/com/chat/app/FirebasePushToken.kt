package com.chat.app

import android.content.Context

internal object FirebasePushToken {
  const val supported = false
  fun read(context: Context, binding: String, done: (AndroidPushRotation?) -> Unit) = done(null)
  fun cancel(context: Context) = Unit
}
