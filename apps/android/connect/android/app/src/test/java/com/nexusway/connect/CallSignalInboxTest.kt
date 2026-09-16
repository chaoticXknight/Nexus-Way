package com.nexusway.connect

import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class CallSignalInboxTest {
    @Test fun burstPreservesOriginalOffer() = runBlocking {
        val inbox = CallSignalInbox()
        repeat(65) { index ->
            assertTrue(inbox.submit(JSONObject().put("sequence", index)))
        }
        repeat(65) { index -> assertEquals(index, inbox.frames.receive().getInt("sequence")) }
    }

    @Test fun overflowIsExplicitAndNeverReplacesOldestFrame() = runBlocking {
        val inbox = CallSignalInbox(2)
        assertTrue(inbox.submit(JSONObject().put("sequence", 0)))
        assertTrue(inbox.submit(JSONObject().put("sequence", 1)))
        assertFalse(inbox.submit(JSONObject().put("sequence", 2)))
        assertEquals(0, inbox.frames.receive().getInt("sequence"))
    }
}