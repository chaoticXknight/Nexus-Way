package com.nexusway.connect

/**
 * Owns serialized validation and one-at-a-time renewal of HIVE sessions.
 * It does not store enrollment keys itself; Store supplies and persists them.
 */

import android.content.Context
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

object SessionManager {
    private val sessionLock = Mutex()

    suspend fun client(
        context: Context,
        enrollment: Enrollment,
        acceptSelfSigned: Boolean = false,
        invalidToken: String? = null,
    ): HiveClient = sessionLock.withLock {
        val store = Store(context.applicationContext)
        val client = HiveClient(enrollment.server, acceptSelfSigned).apply {
            pin = enrollment.serverPin
            token = store.sessionToken
        }
        client.info()

        val currentToken = client.token
        if (currentToken != null && currentToken != invalidToken) {
            try {
                client.whoami()
                return@withLock client
            } catch (_: SessionExpiredException) {}
        }

        client.auth(enrollment.accountId, enrollment.device)
        store.sessionToken = client.token
        client
    }
}