package com.nexusway.connect

import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import javax.net.ssl.SSLException
import org.json.JSONObject

internal fun connectionFailure(error: IOException): HiveException {
    val message = when (error) {
        is SSLException -> "Cannot securely connect to HIVE. Check the server certificate."
        is UnknownHostException -> "Cannot find the HIVE server. Check the server address and your connection."
        is SocketTimeoutException -> "HIVE is not responding. Check your connection and try again."
        is ConnectException -> "Cannot connect to HIVE. The server may be offline. Try again shortly."
        else -> "Connection to HIVE was interrupted. Check your connection and try again."
    }
    return HiveException(message).also { it.initCause(error) }
}

internal fun parseHiveResponse(status: Int, body: String?): JSONObject {
    if (status >= 500) throw HiveException("HIVE is temporarily unavailable (HTTP $status). Try again shortly.")
    val value = body?.let { runCatching { JSONObject(it) }.getOrNull() }
        ?: throw HiveException("The server did not return a valid HIVE response (HTTP $status). Check the server address.")
    if (status !in 200..299 || !value.optBoolean("ok", false)) {
        val message = value.optString("err").ifBlank { "HIVE request failed (HTTP $status)." }
        if (message == "invalid or expired session") throw SessionExpiredException()
        throw HiveException(message)
    }
    return value
}