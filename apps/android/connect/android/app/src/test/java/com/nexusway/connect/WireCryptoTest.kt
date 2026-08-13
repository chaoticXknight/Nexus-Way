package com.nexusway.connect

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class WireCryptoTest {
    @Test
    fun foldKeyAndContentRoundTrip() {
        val recipient = WireKey.generate()
        val foldKey = WireCrypto.generateFoldKey()
        val circleId = "fold-test"
        val epoch = 7L

        val wrapped = WireCrypto.wrapFoldKey(
            foldKey,
            circleId,
            epoch,
            recipient.publicBytes,
        )
        val unwrapped = WireCrypto.unwrapFoldKey(recipient, circleId, epoch, wrapped)
        assertArrayEquals(foldKey, unwrapped)

        val plaintext = "private Fold post".toByteArray()
        val envelope = WireCrypto.encryptFoldContent(foldKey, circleId, epoch, plaintext)
        assertArrayEquals(
            plaintext,
            WireCrypto.decryptFoldContent(foldKey, circleId, envelope),
        )

        assertThrows(Exception::class.java) {
            WireCrypto.decryptFoldContent(foldKey, "another-fold", envelope)
        }
    }
}
