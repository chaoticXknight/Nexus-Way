package com.nexusway.connect

import org.junit.Assert.assertEquals
import org.junit.Test

class PendingMessageNotificationsTest {
    private fun message(id: String, mine: Boolean = false) = DirectMessage(
        id = id, peerAccount = "peer", peerHandle = "peer", body = "hello", sent = 1,
        mine = mine, status = "received",
    )

    @Test fun pendingAlertSurvivesEmptyServerInbox() {
        val pending = listOf(message("first"))
        assertEquals(pending, pendingMessageNotifications(pending, emptyList(), emptySet(), emptySet()))
    }

    @Test fun repeatedDownloadsDoNotDuplicateAlerts() {
        val pending = listOf(message("first"))
        assertEquals(pending, pendingMessageNotifications(pending, pending, emptySet(), emptySet()))
    }

    @Test fun deliveredDeletedAndOutgoingMessagesAreExcluded() {
        val incoming = listOf(message("done"), message("deleted"), message("mine", true), message("new"))
        assertEquals(listOf(message("new")), pendingMessageNotifications(emptyList(), incoming, setOf("done"), setOf("deleted")))
    }
}