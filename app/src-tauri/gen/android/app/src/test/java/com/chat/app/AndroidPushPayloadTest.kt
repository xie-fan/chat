package com.chat.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.util.UUID

class AndroidPushPayloadTest {
  @Test
  fun acceptsOnlyCompleteCanonicalIdentifierPayloads() {
    val binding = UUID.randomUUID().toString()
    val conv = UUID.randomUUID().toString()
    val message = UUID.randomUUID().toString()
    val data = mapOf("binding" to binding, "convId" to conv, "messageId" to message)
    assertEquals(AndroidPushPayload(binding, conv, message), AndroidPushPayload.parse(data))
    assertNull(AndroidPushPayload.parse(data - "binding"))
    assertNull(AndroidPushPayload.parse(data + ("convId" to "1-2-3-4-5")))
  }

  @Test
  fun tracksAlreadyShownMessagesPerAccountWithinBoundedHistory() {
    val recent = RecentNotifications(2)
    val websocket = "account" to "message"
    recent.record(websocket)
    assertTrue(recent.contains("account" to "message"))
    assertFalse(recent.contains("other account" to "message"))
    recent.record("account" to "later")
    recent.record("account" to "latest")
    assertFalse(recent.contains(websocket))
  }
}
