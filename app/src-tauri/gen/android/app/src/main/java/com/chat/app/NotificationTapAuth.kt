package com.chat.app

import android.content.Context
import android.util.Base64
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

internal const val NOTIFICATION_TAP_TOKEN = "notificationTapToken"

internal object NotificationTapAuth {
  private const val PREFS = "notification_tap_auth"
  private const val KEY = "key"
  private const val ALGORITHM = "HmacSHA256"
  private const val KEY_BYTES = 32

  @Synchronized
  fun sign(context: Context, convId: String, account: String): ByteArray {
    val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
    val key = readKey(context) ?: ByteArray(KEY_BYTES).also {
      SecureRandom().nextBytes(it)
      check(prefs.edit().putString(KEY, Base64.encodeToString(it, Base64.NO_WRAP)).commit())
    }
    return tag(key, convId, account)
  }

  fun verify(context: Context, convId: String, account: String, token: ByteArray): Boolean {
    val key = readKey(context) ?: return false
    return matches(key, convId, account, token)
  }

  private fun readKey(context: Context): ByteArray? {
    val encoded = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
      .getString(KEY, null) ?: return null
    return try {
      Base64.decode(encoded, Base64.NO_WRAP).takeIf { it.size == KEY_BYTES }
    } catch (_: IllegalArgumentException) {
      null
    }
  }

  internal fun tag(key: ByteArray, convId: String, account: String): ByteArray {
    val conv = convId.toByteArray(Charsets.UTF_8)
    val mac = Mac.getInstance(ALGORITHM)
    mac.init(SecretKeySpec(key, ALGORITHM))
    mac.update(ByteBuffer.allocate(Int.SIZE_BYTES).putInt(conv.size).array())
    mac.update(conv)
    return mac.doFinal(account.toByteArray(Charsets.UTF_8))
  }

  internal fun matches(key: ByteArray, convId: String, account: String, token: ByteArray): Boolean =
    MessageDigest.isEqual(tag(key, convId, account), token)
}
