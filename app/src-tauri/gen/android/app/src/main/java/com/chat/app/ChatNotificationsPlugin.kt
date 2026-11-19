package com.chat.app

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.Plugin

private const val PERMISSION = "notifications"
private const val CHANNEL = "chat_messages"

@InvokeArg
class ChatNotificationArgs {
  lateinit var title: String
  lateinit var body: String
  var convId: String? = null
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

  @SuppressLint("MissingPermission")
  @Command
  fun show(invoke: Invoke) {
    val args = invoke.parseArgs(ChatNotificationArgs::class.java)
    activity.runOnUiThread {
      if (args.convId != null && (activity as MainActivity).chatResumed && activity.hasWindowFocus()) {
        invoke.resolveObject(false)
        return@runOnUiThread
      }
      if (permission() != "granted") {
        invoke.reject("Notifications are disabled")
        return@runOnUiThread
      }
      try {
        val manager = activity.getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= 26) {
          manager.createNotificationChannel(NotificationChannel(
            CHANNEL, "Chat 消息", NotificationManager.IMPORTANCE_HIGH
          ))
          if (manager.getNotificationChannel(CHANNEL)?.importance == NotificationManager.IMPORTANCE_NONE) {
            invoke.reject("The Chat messages notification channel is disabled")
            return@runOnUiThread
          }
        }
        val intent = Intent(activity, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
        val pending = PendingIntent.getActivity(activity, 0, intent, PendingIntent.FLAG_IMMUTABLE)
        val notification = NotificationCompat.Builder(activity, CHANNEL)
          .setSmallIcon(R.drawable.ic_chat_notification)
          .setContentTitle(args.title)
          .setContentText(args.body)
          .setStyle(NotificationCompat.BigTextStyle().bigText(args.body))
          .setCategory(NotificationCompat.CATEGORY_MESSAGE)
          .setPriority(NotificationCompat.PRIORITY_HIGH)
          .setDefaults(NotificationCompat.DEFAULT_ALL)
          .setAutoCancel(true)
          .setContentIntent(pending)
          .build()
        manager.notify(args.convId ?: "test", 1, notification)
        invoke.resolveObject(true)
      } catch (error: Exception) {
        invoke.reject("Could not post notification", error)
      }
    }
  }
}
