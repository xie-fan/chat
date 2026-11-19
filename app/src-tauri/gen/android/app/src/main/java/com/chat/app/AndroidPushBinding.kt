package com.chat.app

import android.content.Context
import java.util.UUID

internal object AndroidPushBinding {
  private fun prefs(context: Context) = context.getSharedPreferences("chat_android_push", Context.MODE_PRIVATE)

  @Synchronized
  fun update(context: Context, account: String?, server: String?, enabled: Boolean): String? {
    val prefs = prefs(context)
    val current = prefs.getString("binding", null).takeIf {
      prefs.getString("account", null) == account && prefs.getString("server", null) == server
    }
    if (!enabled || current == null) FirebasePushToken.cancel(context)
    if (account == null || server.isNullOrEmpty()) {
      prefs.edit().clear().commit()
      return null
    }
    val binding = current ?: UUID.randomUUID().toString().also { id ->
      val key = (UUID.randomUUID().toString() + UUID.randomUUID()).replace("-", "")
      prefs.edit().clear().putString("account", account).putString("server", server)
        .putString("binding", id).putString("rotationKey", key).commit()
    }
    if (prefs.getBoolean("enabled", false) != enabled) {
      prefs.edit().putBoolean("enabled", enabled).commit()
    }
    return binding.takeIf { enabled }
  }

  @Synchronized
  fun record(
    context: Context, binding: String?, token: String, project: String, observedGeneration: Long? = null,
  ): AndroidPushRotation? {
    val prefs = prefs(context)
    val active = prefs.getString("binding", null) ?: return null
    if (binding != null && binding != active) return null
    if (!prefs.getBoolean("enabled", false)) return null
    val generation = prefs.getLong("generation", 0)
    if (observedGeneration != null && generation > observedGeneration) return rotation(context)
    if (prefs.getString("token", null) != token || prefs.getString("project", null) != project) {
      if (!prefs.edit().putString("token", token).putString("project", project)
        .putLong("generation", generation + 1).commit()) return null
    }
    return rotation(context)
  }

  @Synchronized
  fun rotation(context: Context): AndroidPushRotation? {
    val prefs = prefs(context)
    if (!prefs.getBoolean("enabled", false)) return null
    return AndroidPushRotation(
      prefs.getString("server", null) ?: return null,
      prefs.getString("binding", null) ?: return null,
      prefs.getString("rotationKey", null) ?: return null,
      prefs.getString("project", null) ?: return null,
      prefs.getString("token", null) ?: return null,
      prefs.getLong("generation", 0),
    )
  }

  @Synchronized
  fun account(context: Context, binding: String): String? {
    val prefs = prefs(context)
    if (!prefs.getBoolean("enabled", false) || prefs.getString("binding", null) != binding) return null
    return prefs.getString("account", null)
  }
}

internal data class AndroidPushRotation(
  val server: String,
  val binding: String,
  val rotationKey: String,
  val project: String,
  val token: String,
  val generation: Long,
) {
  fun registration() = mapOf(
    "binding" to binding, "rotationKey" to rotationKey, "project" to project,
    "token" to token, "generation" to generation,
  )
}
