package com.chat.app

import android.Manifest
import android.app.Activity
import android.os.Build
import androidx.core.app.NotificationManagerCompat
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Channel
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

private const val PERMISSION = "notifications"

@InvokeArg
class ChatNotificationArgs {
  lateinit var title: String
  lateinit var body: String
  var convId: String? = null
  var account: String? = null
  var messageId: String? = null
}

@InvokeArg
class AndroidPushArgs {
  var account: String? = null
  var enabled: Boolean = false
}

@InvokeArg
class NotificationOpenListener {
  lateinit var account: String
  lateinit var channel: Channel
}

@TauriPlugin(permissions = [
  Permission(strings = [Manifest.permission.POST_NOTIFICATIONS], alias = PERMISSION)
])
class ChatNotificationsPlugin(private val activity: Activity) : Plugin(activity) {
  private fun permission(): String {
    if (NotificationManagerCompat.from(activity).areNotificationsEnabled()) return "granted"
    return if (Build.VERSION.SDK_INT >= 33 &&
      getPermissionState(PERMISSION) == PermissionState.PROMPT) "default" else "denied"
  }

  @Command
  fun permission(invoke: Invoke) {
    invoke.resolveObject(permission())
  }

  @Command
  fun requestPermission(invoke: Invoke) {
    activity.runOnUiThread {
      val status = permission()
      if (Build.VERSION.SDK_INT >= 33 && status != "granted" &&
        getPermissionState(PERMISSION) != PermissionState.GRANTED) {
        requestPermissionForAlias(PERMISSION, invoke, "permissionResult")
      } else {
        invoke.resolveObject(status)
      }
    }
  }

  @PermissionCallback
  private fun permissionResult(invoke: Invoke) {
    invoke.resolveObject(permission())
  }

  @Command
  fun listenForNotificationOpen(invoke: Invoke) {
    val args = invoke.parseArgs(NotificationOpenListener::class.java)
    activity.runOnUiThread {
      (activity as MainActivity).listenForNotificationOpen(args.account, args.channel)
      invoke.resolve()
    }
  }

  @Command
  fun androidPushRegistration(invoke: Invoke) {
    if (!FirebasePushToken.supported) {
      invoke.resolve()
      return
    }
    val args = invoke.parseArgs(AndroidPushArgs::class.java)
    activity.runOnUiThread {
      val binding = AndroidPushBinding.update(activity, args.account, args.enabled)
      if (binding == null) {
        invoke.resolve()
        return@runOnUiThread
      }
      try {
        FirebasePushToken.read(activity) { project, token ->
          if (project == null || token == null) invoke.resolve()
          else invoke.resolveObject(mapOf("token" to token, "project" to project, "binding" to binding))
        }
      } catch (_: IllegalStateException) {
        invoke.resolve()
      }
    }
  }

  @Command
  fun show(invoke: Invoke) {
    val args = invoke.parseArgs(ChatNotificationArgs::class.java)
    activity.runOnUiThread {
      try {
        invoke.resolveObject(ChatSystemNotification.show(
          activity, args.title, args.body, args.convId, args.account, args.messageId
        ))
      } catch (error: Exception) {
        invoke.reject("Could not post notification", error)
      }
    }
  }
}
