package com.chat.app

import android.os.Handler
import android.os.Looper
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

class ChatFirebaseService : FirebaseMessagingService() {
  override fun onNewToken(token: String) {
    FirebasePushToken.refreshed(token)
  }

  override fun onMessageReceived(message: RemoteMessage) {
    val payload = AndroidPushPayload.parse(message.data) ?: return
    Handler(Looper.getMainLooper()).post {
      val account = AndroidPushBinding.account(this, payload.binding) ?: return@post
      runCatching {
        ChatSystemNotification.show(
          this, getString(R.string.app_name), "你收到一条新消息", payload.convId, account, payload.messageId
        )
      }
    }
  }
}
