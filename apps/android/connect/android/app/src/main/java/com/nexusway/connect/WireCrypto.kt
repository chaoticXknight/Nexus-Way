package com.nexusway.connect

/**
 * Owns direct-message key agreement, authenticated encryption, and canonical
 * signed payload strings. It does not send or store messages; HiveClient and
 * Store own those steps.
 */

import org.bouncycastle.crypto.agreement.X25519Agreement
import org.bouncycastle.crypto.params.X25519PrivateKeyParameters
import org.bouncycastle.crypto.params.X25519PublicKeyParameters
import org.json.JSONObject
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

class WireKey(val seed: ByteArray) {
    private val privateKey = X25519PrivateKeyParameters(seed, 0)
    val publicBytes: ByteArray = privateKey.generatePublicKey().encoded

    fun agree(peerPublic: ByteArray): ByteArray {
        require(peerPublic.size == 32) { "invalid messaging public key" }
        val agreement = X25519Agreement()
        agreement.init(privateKey)
        return ByteArray(agreement.agreementSize).also {
            agreement.calculateAgreement(X25519PublicKeyParameters(peerPublic, 0), it, 0)
        }
    }

    companion object {
        fun generate() = WireKey(ByteArray(32).also(SecureRandom()::nextBytes))
    }
}

data class MessageAttachment(
    val blobId: String,
    val key: String,
    val nonce: String,
    val mime: String,
)

data class EncryptedAttachment(
    val ciphertext: ByteArray,
    val key: String,
    val nonce: String,
)

object WireCrypto {
    private fun hmac(key: ByteArray, data: ByteArray): ByteArray =
        Mac.getInstance("HmacSHA256").run {
            init(SecretKeySpec(key, "HmacSHA256"))
            doFinal(data)
        }

    private fun key(shared: ByteArray, messageId: String): ByteArray {
        val prk = hmac(ByteArray(32), shared)
        return hmac(prk, "nexus-connect-wire:v1:$messageId\u0001".toByteArray())
    }

    fun encrypt(recipientPublic: ByteArray, messageId: String, plaintext: ByteArray): String {
        val ephemeral = WireKey.generate()
        val nonce = ByteArray(12).also(SecureRandom()::nextBytes)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.ENCRYPT_MODE,
            SecretKeySpec(key(ephemeral.agree(recipientPublic), messageId), "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD(messageId.toByteArray())
        return JSONObject().apply {
            put("v", 1)
            put("ephemeral_pub", b64(ephemeral.publicBytes))
            put("nonce", b64(nonce))
            put("ciphertext", b64(cipher.doFinal(plaintext)))
        }.toString()
    }

    fun decrypt(recipient: WireKey, messageId: String, envelope: String): ByteArray {
        val value = JSONObject(envelope)
        require(value.optInt("v") == 1) { "unsupported message version" }
        val nonce = unb64(value.getString("nonce"))
        require(nonce.size == 12) { "invalid message nonce" }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            SecretKeySpec(
                key(recipient.agree(unb64(value.getString("ephemeral_pub"))), messageId),
                "AES",
            ),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD(messageId.toByteArray())
        return cipher.doFinal(unb64(value.getString("ciphertext")))
    }

    fun generateFoldKey(): ByteArray = ByteArray(32).also(SecureRandom()::nextBytes)

    fun wrapFoldKey(
        foldKey: ByteArray,
        circleId: String,
        epoch: Long,
        recipientPublic: ByteArray,
    ): String {
        require(foldKey.size == 32) { "invalid Fold key" }
        return encrypt(recipientPublic, "fold-key:$circleId:$epoch", foldKey)
    }

    fun unwrapFoldKey(
        recipient: WireKey,
        circleId: String,
        epoch: Long,
        envelope: String,
    ): ByteArray = decrypt(recipient, "fold-key:$circleId:$epoch", envelope).also {
        require(it.size == 32) { "invalid Fold key" }
    }

    fun encryptFoldContent(
        foldKey: ByteArray,
        circleId: String,
        epoch: Long,
        plaintext: ByteArray,
    ): String {
        require(foldKey.size == 32) { "invalid Fold key" }
        val nonce = ByteArray(12).also(SecureRandom()::nextBytes)
        val aad = "nexus-connect-fold:v1:$circleId:$epoch".toByteArray()
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.ENCRYPT_MODE,
            SecretKeySpec(foldKey, "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD(aad)
        return JSONObject().apply {
            put("v", 1)
            put("epoch", epoch)
            put("nonce", b64(nonce))
            put("ciphertext", b64(cipher.doFinal(plaintext)))
        }.toString()
    }

    fun decryptFoldContent(foldKey: ByteArray, circleId: String, envelope: String): ByteArray {
        require(foldKey.size == 32) { "invalid Fold key" }
        val value = JSONObject(envelope)
        require(value.optInt("v") == 1) { "unsupported Fold content version" }
        val epoch = value.getLong("epoch")
        val nonce = unb64(value.getString("nonce"))
        require(nonce.size == 12) { "invalid Fold content nonce" }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            SecretKeySpec(foldKey, "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD("nexus-connect-fold:v1:$circleId:$epoch".toByteArray())
        return cipher.doFinal(unb64(value.getString("ciphertext")))
    }

    fun validateDevice(accountId: String, identityPub: String, device: JSONObject): Boolean {
        val identityBytes = runCatching { unb64(identityPub) }.getOrNull() ?: return false
        if (accountIdFor(identityBytes) != accountId) return false
        val devicePub = device.optString("device_pub")
        val certMessage = "hive-device-cert:v1:$devicePub:${device.optString("name")}:${device.optLong("created")}"
        if (!Key.verify(identityBytes, certMessage.toByteArray(), runCatching {
                unb64(device.getString("cert"))
            }.getOrNull() ?: return false)) return false
        val wireMessage = "hive-wire-key:v1:${device.optString("device_id")}:${device.optString("wire_pub")}"
        return Key.verify(
            runCatching { unb64(devicePub) }.getOrNull() ?: return false,
            wireMessage.toByteArray(),
            runCatching { unb64(device.getString("wire_signature")) }.getOrNull() ?: return false,
        )
    }

    fun signedMessage(
        id: String,
        senderAccount: String,
        recipientAccount: String,
        sent: Long,
        body: String,
    ) = "hive-wire-message:v1:$id:$senderAccount:$recipientAccount:$sent:${b64(body.toByteArray())}"

    fun signedMessageV2(
        id: String,
        senderAccount: String,
        recipientAccount: String,
        sent: Long,
        body: String,
        attachment: MessageAttachment,
    ) = listOf(
        "hive-wire-message:v2",
        id,
        senderAccount,
        recipientAccount,
        sent.toString(),
        b64(body.toByteArray()),
        attachment.blobId,
        attachment.key,
        attachment.nonce,
        attachment.mime,
    ).joinToString(":")

    fun encryptAttachment(messageId: String, plaintext: ByteArray): EncryptedAttachment {
        val key = ByteArray(32).also(SecureRandom()::nextBytes)
        val nonce = ByteArray(12).also(SecureRandom()::nextBytes)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.ENCRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD("nexus-connect-wire-attachment:v1:$messageId".toByteArray())
        return EncryptedAttachment(cipher.doFinal(plaintext), b64(key), b64(nonce))
    }

    fun decryptAttachment(
        messageId: String,
        attachment: MessageAttachment,
        ciphertext: ByteArray,
    ): ByteArray {
        val key = unb64(attachment.key)
        val nonce = unb64(attachment.nonce)
        require(key.size == 32 && nonce.size == 12) { "invalid attachment key" }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(128, nonce),
        )
        cipher.updateAAD("nexus-connect-wire-attachment:v1:$messageId".toByteArray())
        return cipher.doFinal(ciphertext)
    }

    fun signedDeletion(
        eventId: String,
        targetId: String,
        senderAccount: String,
        recipientAccount: String,
        deletedAt: Long,
    ) = "hive-wire-delete:v1:$eventId:$targetId:$senderAccount:$recipientAccount:$deletedAt"

    fun signedHistorySync(
        snapshotId: String,
        senderAccount: String,
        publisherDevice: String,
        targetDevice: String,
        snapshotHash: String,
        syncedThrough: Long,
    ) = "hive-wire-history-sync:v1:$snapshotId:$senderAccount:$publisherDevice:" +
        "$targetDevice:$snapshotHash:$syncedThrough"

    fun signedCall(callId: String, action: String, kind: String, payload: String) =
        "hive-call-signal:v1:$callId:$action:$kind:$payload"
}