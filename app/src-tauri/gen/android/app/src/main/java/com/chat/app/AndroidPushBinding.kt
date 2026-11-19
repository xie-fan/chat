package com.chat.app

import android.content.Context
import java.util.UUID

internal object AndroidPushBinding {
  private fun prefs(context: Context) = context.getSharedPreferences("chat_android_push", Context.MODE_PRIVATE)

  @Synchronized
  fun update(context: Context, account: String?, enabled: Boolean): String? {
    val prefs = prefs(context)
    if (account == null) {
      prefs.edit().clear().apply()
      return null
    }
    val current = prefs.getString("binding", null).takeIf { prefs.getString("account", null) == account }
    val binding = current ?: UUID.randomUUID().toString()
    prefs.edit().putString("account", account).putString("binding", binding)
      .putBoolean("enabled", enabled).apply()
    return binding.takeIf { enabled }
  }

  @Synchronized
  fun account(context: Context, binding: String): String? {
    val prefs = prefs(context)
    if (!prefs.getBoolean("enabled", false) || prefs.getString("binding", null) != binding) return null
    return prefs.getString("account", null)
  }
}
