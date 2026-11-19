package com.chat.app

import android.annotation.SuppressLint
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat

private const val CHANNEL = "chat_messages"
internal const val OPEN_CHAT_NOTIFICATION = "com.chat.app.OPEN_CHAT_NOTIFICATION"

internal object ChatSystemNotification {
  var foreground = false
  private val delivered = RecentNotifications()

  @SuppressLint("MissingPermission")
  fun show(context: Context, title: String, body: String, convId: String?, account: String?, messageId: String?): Boolean {
    if (convId != null && foreground) return false
    val key = if (account != null && messageId != null) account to messageId else null
    if (key != null && delivered.contains(key)) return false
    check(NotificationManagerCompat.from(context).areNotificationsEnabled()) { "Notifications are disabled" }
    val manager = context.getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= 26) {
      manager.createNotificationChannel(NotificationChannel(
        CHANNEL, "Chat 消息", NotificationManager.IMPORTANCE_HIGH
      ))
      check(manager.getNotificationChannel(CHANNEL)?.importance != NotificationManager.IMPORTANCE_NONE) {
        "The Chat messages notification channel is disabled"
      }
    }
    val intent = Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
    if (convId != null && account != null) {
      intent.action = OPEN_CHAT_NOTIFICATION
      intent.data = Uri.Builder().scheme("chat-notification").authority("open")
        .appendPath(account).appendPath(convId).build()
      intent.putExtra("convId", convId)
      intent.putExtra("account", account)
      intent.putExtra(NOTIFICATION_TAP_TOKEN, NotificationTapAuth.sign(context, convId, account))
    }
    val pending = PendingIntent.getActivity(
      context, 0, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    val notification = NotificationCompat.Builder(context, CHANNEL)
      .setSmallIcon(R.drawable.ic_chat_notification)
      .setContentTitle(title)
      .setContentText(body)
      .setStyle(NotificationCompat.BigTextStyle().bigText(body))
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setPriority(NotificationCompat.PRIORITY_HIGH)
      .setDefaults(NotificationCompat.DEFAULT_ALL)
      .setAutoCancel(true)
      .setContentIntent(pending)
      .build()
    manager.notify(convId ?: "test", 1, notification)
    if (key != null) delivered.record(key)
    return true
  }
}
