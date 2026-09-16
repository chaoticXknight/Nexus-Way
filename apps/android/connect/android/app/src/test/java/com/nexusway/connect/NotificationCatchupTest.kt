package com.nexusway.connect

import kotlinx.coroutines.runBlocking
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test

class NotificationCatchupTest {
    @Test fun continuesPastFirstPageAndStopsAtPreviousWatermark() = runBlocking {
        val requested = mutableListOf<String?>()
        val rows = notificationCatchup("old") { cursor ->
            requested += cursor
            if (cursor == null) JSONObject().put("notifications", JSONArray().put(JSONObject().put("id", "new")))
                .put("next_cursor", "page2")
            else JSONObject().put("notifications", JSONArray().put(JSONObject().put("id", "middle"))
                .put(JSONObject().put("id", "old"))).put("next_cursor", "page3")
        }
        assertEquals(listOf("new", "middle"), rows.map { it.getString("id") })
        assertEquals(listOf(null, "page2"), requested)
    }
}