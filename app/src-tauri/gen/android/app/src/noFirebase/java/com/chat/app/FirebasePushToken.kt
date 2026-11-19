package com.chat.app

import android.content.Context

internal object FirebasePushToken {
  const val supported = false
  fun read(context: Context, done: (String?, String?) -> Unit) = done(null, null)
}
