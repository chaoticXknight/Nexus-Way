package com.nexusway.connect

/**
 * Owns the in-process queue that hands live HIVE frames to the visible Connect UI.
 * Despite this file's old name, it does not define or run an Android service;
 * Nexus Notify owns the persistent background connection.
 */

import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.receiveAsFlow
import org.json.JSONObject

object HiveStreamEvents {
    private val pendingFrames = Channel<JSONObject>(
        capacity = 64,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )
    val frames = pendingFrames.receiveAsFlow()

    fun tryEmit(frame: JSONObject): Boolean {
        if (frame.optString("type") == "call_signal") {
            CallSession.receive(frame)
            return true
        }
        return pendingFrames.trySend(frame).isSuccess
    }
}