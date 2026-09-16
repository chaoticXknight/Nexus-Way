package com.nexusway.connect

import org.json.JSONObject

internal suspend fun notificationCatchup(
    lastId: String?,
    fetch: suspend (String?) -> JSONObject,
): List<JSONObject> {
    val notifications = mutableListOf<JSONObject>()
    val cursors = mutableSetOf<String>()
    var cursor: String? = null
    do {
        val page = fetch(cursor)
        val rows = page.optJSONArray("notifications") ?: break
        if (rows.length() == 0) break
        for (index in 0 until rows.length()) {
            val row = rows.getJSONObject(index)
            if (row.optString("id") == lastId) return notifications
            notifications += row
        }
        cursor = page.optString("next_cursor").takeUnless { it.isBlank() || it == "null" }
    } while (cursor != null && cursors.add(cursor))
    return notifications
}