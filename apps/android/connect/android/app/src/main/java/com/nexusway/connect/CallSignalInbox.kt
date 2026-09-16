package com.nexusway.connect

import kotlinx.coroutines.channels.Channel
import org.json.JSONObject

internal class CallSignalInbox(capacity: Int = 256) {
    val frames = Channel<JSONObject>(capacity)
    fun submit(frame: JSONObject): Boolean = frames.trySend(frame).isSuccess
}