package com.chat.app

import java.util.UUID

internal class RecentNotifications(private val limit: Int = 512) {
  private val keys = LinkedHashSet<Pair<String, String>>()

  fun contains(key: Pair<String, String>) = key in keys

  fun record(key: Pair<String, String>) {
    if (keys.add(key) && keys.size > limit) keys.remove(keys.first())
  }
}

internal data class AndroidPushPayload(val binding: String, val convId: String, val messageId: String) {
  companion object {
    fun parse(data: Map<String, String>): AndroidPushPayload? {
      val binding = data["binding"].canonicalUuid() ?: return null
      val convId = data["convId"].canonicalUuid() ?: return null
      val messageId = data["messageId"].canonicalUuid() ?: return null
      return AndroidPushPayload(binding, convId, messageId)
    }

    private fun String?.canonicalUuid(): String? {
      if (this == null || length != 36) return null
      return try {
        UUID.fromString(this).toString().takeIf { it == this }
      } catch (_: IllegalArgumentException) {
        null
      }
    }
  }
}
