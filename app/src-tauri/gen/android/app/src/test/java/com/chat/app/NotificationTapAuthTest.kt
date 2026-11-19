package com.chat.app

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class NotificationTapAuthTest {
  private val key = ByteArray(32) { it.toByte() }

  @Test
  fun tokenAuthenticatesTheExactConversationAndAccount() {
    val token = NotificationTapAuth.tag(key, "abc", "def")
    assertTrue(NotificationTapAuth.matches(key, "abc", "def", token))
    assertFalse(NotificationTapAuth.matches(key, "different", "def", token))
    assertFalse(NotificationTapAuth.matches(key, "abc", "other-server-account", token))
    assertFalse(NotificationTapAuth.matches(key, "ab", "cdef", token))
  }

  @Test
  fun tokenFromAnotherKeyOrTruncatedTokenDoesNotAuthenticate() {
    val foreign = NotificationTapAuth.tag(ByteArray(32) { 9 }, "abc", "def")
    val truncated = NotificationTapAuth.tag(key, "abc", "def").copyOf(12)
    assertFalse(NotificationTapAuth.matches(key, "abc", "def", foreign))
    assertFalse(NotificationTapAuth.matches(key, "abc", "def", truncated))
  }
}
