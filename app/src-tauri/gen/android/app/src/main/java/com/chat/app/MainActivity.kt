package com.chat.app

import android.content.Intent
import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import app.tauri.plugin.Channel

class MainActivity : TauriActivity() {
  private var pendingNotification: NotificationOpen? = null
  private var notificationListener: Pair<String, Channel>? = null

  var chatResumed = false
    private set

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    if (savedInstanceState == null) handleNotificationIntent(intent)
    // 全面屏下 WebView 会画到状态栏/导航栏/键盘底下；按系统栏和键盘让出内边距
    val root = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(root) { v, insets ->
      val b = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.ime())
      v.setPadding(b.left, b.top, b.right, b.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }

  override fun onNewIntent(intent: Intent) {
    super.onNewIntent(intent)
    handleNotificationIntent(intent)
  }

  fun listenForNotificationOpen(account: String, channel: Channel) {
    notificationListener = account to channel
    pendingNotification?.let {
      pendingNotification = null
      if (it.account == account) channel.sendObject(it.convId)
    }
  }

  private fun handleNotificationIntent(intent: Intent) {
    if (intent.action != OPEN_CHAT_NOTIFICATION) return
    val convId = intent.getStringExtra("convId") ?: return
    val account = intent.getStringExtra("account") ?: return
    val token = intent.getByteArrayExtra(NOTIFICATION_TAP_TOKEN) ?: return
    if (convId.isEmpty() || account.isEmpty() || !NotificationTapAuth.verify(this, convId, account, token)) return
    val listener = notificationListener
    if (listener == null) {
      pendingNotification = NotificationOpen(convId, account)
    } else if (listener.first == account) {
      listener.second.sendObject(convId)
    }
  }

  override fun onResume() {
    super.onResume()
    chatResumed = true
  }

  override fun onPause() {
    chatResumed = false
    super.onPause()
  }
}

private data class NotificationOpen(val convId: String, val account: String)
