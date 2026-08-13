package com.nexusway.connect

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MessageHistoryMergeTest {
    private fun message(id: String, sent: Long, status: String) = DirectMessage(
        id = id,
        peerAccount = "peer",
        peerHandle = "peer",
        body = id,
        sent = sent,
        mine = true,
        status = status,
    )

    @Test
    fun tombstonesWinAndStrongestStatusSurvives() {
        val kept = "11111111111111111111111111111111"
        val deleted = "22222222222222222222222222222222"
        val remoteOnly = "33333333333333333333333333333333"

        val merged = mergeMessageHistories(
            local = listOf(message(kept, 10, "sent"), message(deleted, 20, "read")),
            remote = listOf(message(kept, 10, "read"), message(remoteOnly, 30, "received")),
            localDeleted = emptySet(),
            remoteDeleted = setOf(deleted),
        )

        assertEquals(listOf(kept, remoteOnly), merged.messages.map { it.id })
        assertEquals("read", merged.messages.first { it.id == kept }.status)
        assertTrue(deleted in merged.deletedIds)
        assertFalse(merged.messages.any { it.id == deleted })
    }

    @Test
    fun latestConversationVisibilityWinsAcrossDevices() {
        val merged = mergeConversationVisibilityStates(
            local = mapOf(
                "old-hidden" to (true to 100L),
                "reopened" to (true to 100L),
            ),
            remote = mapOf(
                "old-hidden" to (false to 90L),
                "reopened" to (false to 110L),
                "remote-hidden" to (true to 120L),
            ),
        )

        assertTrue(merged.getValue("old-hidden").first)
        assertFalse(merged.getValue("reopened").first)
        assertTrue(merged.getValue("remote-hidden").first)
    }
}
