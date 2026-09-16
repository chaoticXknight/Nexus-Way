package com.nexusway.connect

import java.net.UnknownHostException
import javax.net.ssl.SSLHandshakeException
import org.junit.Assert.*
import org.junit.Test

class HiveResponsesTest {
    @Test fun proxyOutageIsNotAParserError() {
        val error = runCatching { parseHiveResponse(502, "<html>Bad gateway</html>") }.exceptionOrNull()
        assertTrue(error is HiveException)
        assertTrue(error!!.message!!.contains("temporarily unavailable"))
    }

    @Test fun malformedSuccessIsExplained() {
        val error = runCatching { parseHiveResponse(200, "not json") }.exceptionOrNull()
        assertTrue(error is HiveException)
        assertTrue(error!!.message!!.contains("valid HIVE response"))
    }

    @Test fun httpFailureCannotBeReportedAsSuccess() {
        assertTrue(runCatching { parseHiveResponse(403, "{\"ok\":true}") }.isFailure)
    }

    @Test fun sessionRenewalRemainsTyped() {
        val error = runCatching {
            parseHiveResponse(200, "{\"ok\":false,\"err\":\"invalid or expired session\"}")
        }.exceptionOrNull()
        assertTrue(error is SessionExpiredException)
        assertTrue(parseHiveResponse(200, "{\"ok\":true}").getBoolean("ok"))
    }

    @Test fun dnsAndTlsFailuresStayDistinct() {
        val dns = UnknownHostException("internal details")
        assertTrue(connectionFailure(dns).message!!.contains("Cannot find"))
        assertSame(dns, connectionFailure(dns).cause)
        assertTrue(connectionFailure(SSLHandshakeException("bad certificate")).message!!.contains("securely"))
    }
}