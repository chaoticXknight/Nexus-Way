// Owns Connect's HTTP/WebSocket HIVE protocol client and signing primitives.
// It does not own screen state or encrypted local persistence; those belong to
// ConnectViewModel and Store. It mirrors the Rust hive-client protocol:
//
//   device cert:  "hive-device-cert:v1:{device_pub_b64}:{name}:{created}"
//   auth finish:  "hive-auth:v1:{challenge_b64}:{server_pub_b64}:{timestamp}"
//
// Base64 = standard RFC 4648 with padding (nexus-common b64). Trust model:
// TOFU pin of the server's Ed25519 public key from /v1/info — certificate
// validation is only relaxed for --dev servers (self-signed), and even then
// the pin is what carries trust, exactly like the desktop clients.

package com.nexusway.connect

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.bouncycastle.crypto.params.Ed25519PrivateKeyParameters
import org.bouncycastle.crypto.params.Ed25519PublicKeyParameters
import org.bouncycastle.crypto.signers.Ed25519Signer
import org.json.JSONArray
import org.json.JSONObject
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.X509Certificate
import java.util.Base64
import java.util.concurrent.TimeUnit
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager

const val PROTOCOL_VERSION = 1

open class HiveException(message: String) : Exception(message)
class SessionExpiredException : HiveException("invalid or expired session")

data class CallIceServer(
    val urls: List<String>,
    val username: String = "",
    val credential: String = "",
)

/** An Ed25519 keypair from a 32-byte seed (identity or device key). */
class Key(val seed: ByteArray) {
    private val priv = Ed25519PrivateKeyParameters(seed, 0)
    val publicBytes: ByteArray =
        (priv.generatePublicKey() as Ed25519PublicKeyParameters).encoded

    fun sign(message: ByteArray): ByteArray {
        val signer = Ed25519Signer()
        signer.init(true, priv)
        signer.update(message, 0, message.size)
        return signer.generateSignature()
    }

    companion object {
        fun generate(): Key {
            val seed = ByteArray(32)
            SecureRandom().nextBytes(seed)
            return Key(seed)
        }

        fun verify(publicKey: ByteArray, message: ByteArray, signature: ByteArray): Boolean =
            runCatching {
                val verifier = Ed25519Signer()
                verifier.init(false, Ed25519PublicKeyParameters(publicKey, 0))
                verifier.update(message, 0, message.size)
                verifier.verifySignature(signature)
            }.getOrDefault(false)
    }
}

fun b64(data: ByteArray): String = Base64.getEncoder().encodeToString(data)
fun unb64(s: String): ByteArray = Base64.getDecoder().decode(s)
fun sha256hex(data: ByteArray): String =
    MessageDigest.getInstance("SHA-256").digest(data).joinToString("") { "%02x".format(it) }

/** Streaming SHA-256 of a file (APKs are too big to slurp on every check). */
fun sha256hexFile(f: java.io.File): String {
    val md = MessageDigest.getInstance("SHA-256")
    f.inputStream().use { ins ->
        val buf = ByteArray(1 shl 16)
        while (true) {
            val n = ins.read(buf)
            if (n <= 0) break
            md.update(buf, 0, n)
        }
    }
    return md.digest().joinToString("") { "%02x".format(it) }
}

/** Self-certifying account id (§1.2). */
fun accountIdFor(publicKey: ByteArray): String = sha256hex(publicKey)
fun deviceIdFor(publicKey: ByteArray): String = sha256hex(publicKey)

class HiveClient(baseUrl: String, acceptSelfSigned: Boolean) {
    private val base = baseUrl.trimEnd('/')
    private val json = "application/json; charset=utf-8".toMediaType()
    private val insecureDevTls = acceptSelfSigned && BuildConfig.DEBUG &&
        runCatching { java.net.URI(base).host }
            .getOrNull() in setOf("127.0.0.1", "localhost", "10.0.2.2")

    @Volatile var pin: String? = null
    @Volatile var token: String? = null

    /** Shared by API calls and Coil image loading (auth header included). */
    val http: OkHttpClient = OkHttpClient.Builder()
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .pingInterval(20, TimeUnit.SECONDS)
        .apply {
            if (insecureDevTls) {
                // --dev servers only; trust is carried by the TOFU pin.
                val tm = object : X509TrustManager {
                    override fun checkClientTrusted(c: Array<X509Certificate>, a: String) {}
                    override fun checkServerTrusted(c: Array<X509Certificate>, a: String) {}
                    override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
                }
                val ctx = SSLContext.getInstance("TLS")
                ctx.init(null, arrayOf(tm), SecureRandom())
                sslSocketFactory(ctx.socketFactory, tm)
                hostnameVerifier { _, _ -> true }
            }
        }
        .addInterceptor { chain ->
            val t = token
            val req = chain.request()
            // API methods set their own header; this catches image loads.
            if (t != null && req.header("Authorization") == null) {
                chain.proceed(req.newBuilder().header("Authorization", "Bearer $t").build())
            } else {
                chain.proceed(req)
            }
        }
        .build()

    // ------------------------------------------------------------ plumbing

    private suspend fun call(request: Request): JSONObject = withContext(Dispatchers.IO) {
        val resp: Response = http.newCall(request).execute()
        resp.use {
            val body = it.body?.string() ?: throw HiveException("empty response")
            val v = JSONObject(body)
            if (!v.optBoolean("ok", false)) {
                val error = v.optString("err", "HIVE error ${it.code}")
                if (error == "invalid or expired session") throw SessionExpiredException()
                throw HiveException(error)
            }
            v
        }
    }

    private suspend fun post(path: String, body: JSONObject): JSONObject =
        call(Request.Builder().url("$base$path").post(body.toString().toRequestBody(json)).build())

    private fun authedBuilder(path: String): Request.Builder {
        val t = token ?: throw HiveException("not signed in")
        return Request.Builder().url("$base$path").header("Authorization", "Bearer $t")
    }

    private suspend fun postAuthed(path: String, body: JSONObject): JSONObject =
        call(authedBuilder(path).post(body.toString().toRequestBody(json)).build())

    private suspend fun getAuthed(path: String): JSONObject =
        call(authedBuilder(path).get().build())

    suspend fun logout() {
        postAuthed("/v1/identity/logout", JSONObject())
        token = null
    }

    /** Mint a device-bound credential that can only open the notification wake stream. */
    suspend fun notificationRelayToken(): String =
        postAuthed("/v1/notification/token", JSONObject()).getString("token")

    suspend fun revokeNotificationRelay() {
        postAuthed("/v1/notification/revoke", JSONObject())
    }

    // -------------------------------------------------------------- updates

    /** Server APK fingerprint for the in-app update check: {sha256, size}. */
    suspend fun appVersion(): JSONObject = getAuthed("/v1/app/version")

    /** Stream the release APK to `dest` (in-app update download). */
    suspend fun downloadApk(
        dest: java.io.File,
        onProgress: (downloaded: Long, total: Long) -> Unit = { _, _ -> },
    ) = downloadApk("/download/connect.apk", dest, onProgress)

    /** Stream the Nexus Notify companion APK to `dest`. */
    suspend fun downloadNotifierApk(
        dest: java.io.File,
        onProgress: (downloaded: Long, total: Long) -> Unit = { _, _ -> },
    ) = downloadApk("/download/nexus-notify.apk", dest, onProgress)

    private suspend fun downloadApk(
        path: String,
        dest: java.io.File,
        onProgress: (downloaded: Long, total: Long) -> Unit,
    ): Unit = withContext(Dispatchers.IO) {
        val req = authedBuilder(path).get().build()
        http.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) throw HiveException("download failed (${resp.code})")
            val body = resp.body ?: throw HiveException("empty download")
            val total = body.contentLength().coerceAtLeast(0L)
            body.byteStream().use { input ->
                dest.outputStream().use { out ->
                    val buf = ByteArray(DEFAULT_BUFFER_SIZE)
                    var downloaded = 0L
                    onProgress(downloaded, total)
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        out.write(buf, 0, n)
                        downloaded += n
                        onProgress(downloaded, total)
                    }
                }
            }
        }
    }

    // ---------------------------------------------------------- info / auth

    /** /v1/info with TOFU pin enforcement (§2.3). */
    suspend fun info(): JSONObject {
        val v = call(Request.Builder().url("$base/v1/info").get().build())
        if (v.optInt("min_client", 1) > PROTOCOL_VERSION) {
            throw HiveException("server requires a newer app version")
        }
        val serverPub = v.getString("server_pub")
        val pinned = pin
        if (pinned != null && pinned != serverPub) {
            throw HiveException("HIVE server key CHANGED — refusing to continue")
        }
        pin = serverPub
        return v
    }

    /** Register (§1.4). Returns (accountId, deviceId). */
    suspend fun register(
        identity: Key,
        device: Key,
        handle: String,
        deviceName: String,
        inviteCode: String? = null,
    ): Pair<String, String> {
        val devicePub = b64(device.publicBytes)
        val created = System.currentTimeMillis() / 1000
        val certMsg = "hive-device-cert:v1:$devicePub:$deviceName:$created"
        val cert = b64(identity.sign(certMsg.toByteArray()))
        val v = post("/v1/identity/register", JSONObject().apply {
            put("identity_pub", b64(identity.publicBytes))
            put("handle", handle)
            put("invite_code", inviteCode ?: JSONObject.NULL)
            put("device_pub", devicePub)
            put("device_name", deviceName)
            put("device_created", created)
            put("device_cert", cert)
            // Age gate: the sign-up UI collects an 18-or-older attestation
            // before this call is reachable.
            put("age_confirmed", true)
        })
        return v.getString("account_id") to v.getString("device_id")
    }

    /** Challenge–response sign-in (§1.5); stores and returns the session token. */
    suspend fun auth(accountId: String, device: Key): String {
        val deviceId = deviceIdFor(device.publicBytes)
        val begin = post("/v1/identity/auth_begin", JSONObject().apply {
            put("account_id", accountId)
            put("device_id", deviceId)
        })
        val challenge = begin.getString("challenge")
        val serverPub = pin ?: throw HiveException("call info() before auth()")
        val ts = System.currentTimeMillis() / 1000
        val msg = "hive-auth:v1:$challenge:$serverPub:$ts"
        val sig = b64(device.sign(msg.toByteArray()))
        val finish = post("/v1/identity/auth_finish", JSONObject().apply {
            put("account_id", accountId)
            put("device_id", deviceId)
            put("challenge", challenge)
            put("timestamp", ts)
            put("sig", sig)
        })
        val t = finish.getString("token")
        token = t
        return t
    }

    suspend fun whoami(): JSONObject = getAuthed("/v1/identity/whoami")

    // ---------------------------------------------------------------- legal

    /** Current ToS/PP versions + this account's acceptance state. */
    suspend fun legalStatus(): JSONObject = getAuthed("/v1/legal/status")

    /** Record explicit acceptance of the current terms + privacy policy. */
    suspend fun legalAccept(): JSONObject =
        postAuthed("/v1/legal/accept", JSONObject().apply {
            put("docs", org.json.JSONArray(listOf("terms", "privacy")))
        })

    /** Fetch a served legal document as plain text (unauthenticated). */
    suspend fun legalDocument(doc: String): String = withContext(Dispatchers.IO) {
        val path = if (doc == "privacy") "/legal/privacy" else "/legal/terms"
        val req = Request.Builder().url("$base$path").get().build()
        http.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) throw HiveException("could not load document (${resp.code})")
            resp.body?.string() ?: throw HiveException("empty document")
        }
    }

    // ---------------------------------------------------------- device link

    /** §4.2 step 1 (new device): announce our pubkey, get the short code. */
    suspend fun linkBegin(device: Key, deviceName: String): String {
        val v = post("/v1/identity/link_begin", JSONObject().apply {
            put("device_pub", b64(device.publicBytes))
            put("device_name", deviceName)
        })
        return v.getString("code")
    }

    /** §4.2 step 4: poll; returns account_id once an enrolled device approves. */
    suspend fun linkStatus(code: String): String? {
        val v = post("/v1/identity/link_status", JSONObject().put("code", code))
        return if (v.optBoolean("approved", false)) v.getString("account_id") else null
    }

    // ----------------------------------------------------- recovery escrow

    /** Park a client-encrypted recovery bundle on the server (§1.7). */
    suspend fun escrowSet(path: String, blob: ByteArray) {
        postAuthed("/v1/identity/escrow_set", JSONObject().apply {
            put("path", path)
            put("blob", b64(blob))
        })
    }

    /** Fetch the ciphertext bundle for handle+path. Rate-limited 5/day. */
    suspend fun escrowFetch(handle: String, path: String): Pair<String, ByteArray> {
        val v = post("/v1/identity/escrow_fetch", JSONObject().apply {
            put("handle", handle)
            put("path", path)
        })
        return v.getString("account_id") to unb64(v.getString("blob"))
    }

    /** Enroll this device using a recovered identity key. Returns device_id. */
    suspend fun recoverDevice(accountId: String, identity: Key, device: Key, deviceName: String): String {
        val devicePub = b64(device.publicBytes)
        val created = System.currentTimeMillis() / 1000
        val certMsg = "hive-device-cert:v1:$devicePub:$deviceName:$created"
        val cert = b64(identity.sign(certMsg.toByteArray()))
        val v = post("/v1/identity/recover_device", JSONObject().apply {
            put("account_id", accountId)
            put("device_pub", devicePub)
            put("device_name", deviceName)
            put("device_created", created)
            put("device_cert", cert)
        })
        return v.getString("device_id")
    }

    /** The account's device list. */
    suspend fun devices(): JSONObject = getAuthed("/v1/identity/devices")

    /** Revoke a device and kill its sessions. */
    suspend fun deviceRevoke(deviceId: String) {
        postAuthed("/v1/identity/device_revoke", JSONObject().put("device_id", deviceId))
    }

    /** Permanently delete the account and all its data (GDPR Art. 17). */
    suspend fun accountDelete() {
        postAuthed("/v1/identity/account_delete", JSONObject())
    }

    /** Everything the server holds about this account, as one JSON blob. */
    suspend fun exportData(): JSONObject = getAuthed("/v1/connect/export")

    /** Accounts this user has blocked. */
    suspend fun blockedList(): JSONObject = getAuthed("/v1/connect/blocked")

    // --------------------------------------------------------------- blobs

    /** Chunked, resumable, content-addressed upload (§3.2). Returns blob id. */
    suspend fun blobUpload(
        data: ByteArray,
        public: Boolean = false,
        purpose: String = "general",
    ): String {
        val hash = sha256hex(data)
        val begin = postAuthed("/v1/blob/begin", JSONObject().apply {
            put("bytes", data.size)
            put("hash", hash)
            put("public", public)
            put("purpose", purpose)
        })
        if (begin.optBoolean("complete", false)) return hash // dedupe
        var offset = begin.optLong("offset", 0)
        val chunkMax = begin.optInt("chunk_max", 1 shl 20).coerceAtMost(1 shl 20)
        while (offset < data.size) {
            val end = minOf(offset + chunkMax, data.size.toLong()).toInt()
            val body = data.copyOfRange(offset.toInt(), end)
                .toRequestBody("application/octet-stream".toMediaType())
            val v = call(
                authedBuilder("/v1/blob/$hash/chunk?offset=$offset").post(body).build()
            )
            offset = v.getLong("offset")
        }
        postAuthed("/v1/blob/$hash/commit", JSONObject())
        return hash
    }

    /** Audience-gated media URL — Coil fetches it with the auth interceptor. */
    fun mediaUrl(blobId: String): String = "$base/v1/connect/media/$blobId"

    suspend fun mediaFetch(blobId: String): ByteArray = withContext(Dispatchers.IO) {
        http.newCall(authedBuilder("/v1/connect/media/$blobId").get().build()).execute().use {
            if (!it.isSuccessful) throw HiveException("media download failed")
            it.body?.bytes() ?: throw HiveException("empty media")
        }
    }

    suspend fun wireAttachmentFetch(blobId: String): ByteArray = withContext(Dispatchers.IO) {
        val response = http.newCall(
            authedBuilder("/v1/wire/attachment/$blobId").get().build(),
        ).execute()
        response.use {
            if (!it.isSuccessful) throw HiveException("attachment download failed")
            it.body?.bytes() ?: throw HiveException("empty attachment")
        }
    }

    // --------------------------------------------------------------- wire

    suspend fun wirePublish(device: Key, wire: WireKey) {
        val deviceId = deviceIdFor(device.publicBytes)
        val wirePub = b64(wire.publicBytes)
        val signature = b64(device.sign("hive-wire-key:v1:$deviceId:$wirePub".toByteArray()))
        postAuthed("/v1/wire/key", JSONObject().apply {
            put("wire_pub", wirePub)
            put("signature", signature)
        })
    }

    suspend fun wireRequest(target: String): String =
        postAuthed("/v1/wire/request", JSONObject().put("target", target)).getString("state")

    suspend fun wireRespond(target: String, accept: Boolean): String =
        postAuthed("/v1/wire/respond", JSONObject().apply {
            put("target", target)
            put("accept", accept)
        }).getString("state")

    suspend fun wireConversations(): JSONObject = getAuthed("/v1/wire/conversations")

    suspend fun wireDirectory(target: String): JSONObject =
        postAuthed("/v1/wire/directory", JSONObject().put("target", target))

    suspend fun wireSend(
        recipientDevice: String,
        messageId: String,
        envelope: String,
        attachmentBlob: String? = null,
    ) {
        postAuthed("/v1/wire/send", JSONObject().apply {
            put("recipient_device", recipientDevice)
            put("msg_id", messageId)
            put("envelope", envelope)
            put("attachment_blob", attachmentBlob ?: JSONObject.NULL)
        })
    }

    suspend fun wireCallSignal(
        target: String,
        callId: String,
        action: String,
        kind: String,
        device: Key,
        payload: JSONObject = JSONObject(),
    ) {
        val encodedPayload = b64(payload.toString().toByteArray())
        val signature = b64(device.sign(
            WireCrypto.signedCall(callId, action, kind, encodedPayload).toByteArray(),
        ))
        postAuthed("/v1/wire/call_signal", JSONObject().apply {
            put("target", target)
            put("call_id", callId)
            put("action", action)
            put("kind", kind)
            put("payload", encodedPayload)
            put("signature", signature)
        })
    }

    suspend fun callIceServers(): List<CallIceServer> {
        val servers = getAuthed("/v1/wire/call_ice").optJSONArray("ice_servers")
            ?: return emptyList()
        return (0 until servers.length()).mapNotNull { index ->
            val server = servers.optJSONObject(index) ?: return@mapNotNull null
            val urls = server.optJSONArray("urls")?.let { array ->
                (0 until array.length()).mapNotNull { array.optString(it).takeIf(String::isNotEmpty) }
            }.orEmpty()
            urls.takeIf { it.isNotEmpty() }?.let {
                CallIceServer(it, server.optString("username"), server.optString("credential"))
            }
        }
    }

    suspend fun wireInbox(): JSONObject = getAuthed("/v1/wire/inbox")

    suspend fun wireAck(messageIds: List<String>) {
        if (messageIds.isNotEmpty()) {
            postAuthed("/v1/wire/ack", JSONObject().put("msg_ids", JSONArray(messageIds)))
        }
    }

    suspend fun wireReceipts(messageIds: List<String>): JSONObject =
        postAuthed("/v1/wire/receipts", JSONObject().put("msg_ids", JSONArray(messageIds)))

    suspend fun wireRead(messageIds: List<String>) {
        if (messageIds.isNotEmpty()) {
            postAuthed("/v1/wire/read", JSONObject().put("msg_ids", JSONArray(messageIds)))
        }
    }

    suspend fun wireRetract(messageId: String) {
        postAuthed("/v1/wire/retract", JSONObject().put("msg_id", messageId))
    }

    suspend fun wireHistorySyncRequest() {
        postAuthed("/v1/wire/history_sync_request", JSONObject())
    }

    suspend fun wireHistorySyncPublish(
        snapshotId: String,
        targetDevice: String,
        snapshotHash: String,
        snapshot: String,
        envelope: String,
        syncedThrough: Long,
    ) {
        postAuthed("/v1/wire/history_sync_publish", JSONObject().apply {
            put("snapshot_id", snapshotId)
            put("target_device", targetDevice)
            put("snapshot_hash", snapshotHash)
            put("snapshot", snapshot)
            put("envelope", envelope)
            put("synced_through", syncedThrough)
        })
    }

    suspend fun wireHistorySyncOffers(): JSONObject =
        getAuthed("/v1/wire/history_sync_offers")

    suspend fun wireHistorySyncConsume(snapshotId: String, syncedThrough: Long) {
        postAuthed("/v1/wire/history_sync_consume", JSONObject().apply {
            put("snapshot_id", snapshotId)
            put("synced_through", syncedThrough)
        })
    }

    // -------------------------------------------------------------- connect

    suspend fun profileSet(displayName: String, bio: String, avatarBlob: String? = null) {
        postAuthed("/v1/connect/profile_set", JSONObject().apply {
            put("display_name", displayName)
            put("bio", bio)
            put("avatar_blob", avatarBlob ?: JSONObject.NULL)
        })
    }

    suspend fun profileGet(target: String): JSONObject =
        postAuthed("/v1/connect/profile_get", JSONObject().put("target", target))

    suspend fun followRequest(target: String) {
        postAuthed("/v1/connect/follow_request", JSONObject().put("target", target))
    }

    suspend fun followAccept(target: String) {
        postAuthed("/v1/connect/follow_accept", JSONObject().put("target", target))
    }

    suspend fun followDecline(target: String) {
        postAuthed("/v1/connect/follow_decline", JSONObject().put("target", target))
    }

    suspend fun unfollow(target: String) {
        postAuthed("/v1/connect/unfollow", JSONObject().put("target", target))
    }

    suspend fun follows(): JSONObject = getAuthed("/v1/connect/follows")

    suspend fun block(target: String) {
        postAuthed("/v1/connect/block", JSONObject().put("target", target))
    }

    suspend fun unblock(target: String) {
        postAuthed("/v1/connect/unblock", JSONObject().put("target", target))
    }

    suspend fun search(q: String): JSONObject =
        postAuthed("/v1/connect/search", JSONObject().put("q", q))

    suspend fun notifications(beforeCursor: String?, limit: Int): JSONObject =
        postAuthed("/v1/connect/notifications", JSONObject().apply {
            put("before_cursor", beforeCursor ?: JSONObject.NULL)
            put("limit", limit)
        })

    suspend fun notificationsSeen() {
        postAuthed("/v1/connect/notifications_seen", JSONObject())
    }

    suspend fun notificationDelete(id: String) {
        postAuthed("/v1/connect/notification_delete", JSONObject().put("id", id))
    }

    suspend fun postCreate(
        kind: String,
        body: String,
        audience: String,
        media: List<String> = emptyList(),
        mediaTypes: List<String> = emptyList(),
        altText: String = "",
        contentWarning: String = "",
        foldEpoch: Long? = null,
    ): String {
        val v = postAuthed("/v1/connect/post_create", JSONObject().apply {
            put("kind", kind)
            put("body", body)
            put("media", JSONArray(media))
            put("media_types", JSONArray(mediaTypes))
            put("alt_text", altText)
            put("content_warning", contentWarning)
            put("audience", audience)
            put("fold_epoch", foldEpoch ?: JSONObject.NULL)
        })
        return v.getString("post_id")
    }

    suspend fun postDelete(postId: String) {
        postAuthed("/v1/connect/post_delete", JSONObject().put("post_id", postId))
    }

    /** Right to correction: edit your own post's text. */
    suspend fun postEdit(postId: String, body: String) {
        postAuthed("/v1/connect/post_edit", JSONObject().put("post_id", postId).put("body", body))
    }

    /** Right to correction: edit your own comment. */
    suspend fun commentEdit(commentId: String, body: String) {
        postAuthed("/v1/connect/comment_edit", JSONObject().put("comment_id", commentId).put("body", body))
    }

    suspend fun commentDelete(commentId: String) {
        postAuthed("/v1/connect/comment_delete", JSONObject().put("comment_id", commentId))
    }

    /** Chronological feed page; cursor from the previous page or null. */
    suspend fun feed(beforeCursor: String?, limit: Int): JSONObject =
        postAuthed("/v1/connect/feed", JSONObject().apply {
            put("before_cursor", beforeCursor ?: JSONObject.NULL)
            put("limit", limit)
        })

    suspend fun foldFeed(circleId: String, beforeCursor: String?, limit: Int): JSONObject =
        postAuthed("/v1/connect/fold_feed", JSONObject().apply {
            put("circle_id", circleId)
            put("before_cursor", beforeCursor ?: JSONObject.NULL)
            put("limit", limit)
        })

    suspend fun authorPosts(target: String, beforeCursor: String?, limit: Int): JSONObject =
        postAuthed("/v1/connect/author_posts", JSONObject().apply {
            put("target", target)
            put("before_cursor", beforeCursor ?: JSONObject.NULL)
            put("limit", limit)
        })

    suspend fun postSave(postId: String) {
        postAuthed("/v1/connect/post_save", JSONObject().put("post_id", postId))
    }

    suspend fun postUnsave(postId: String) {
        postAuthed("/v1/connect/post_unsave", JSONObject().put("post_id", postId))
    }

    suspend fun savedPosts(): JSONObject = getAuthed("/v1/connect/saved_posts")

    suspend fun postPin(postId: String, pinned: Boolean) {
        postAuthed("/v1/connect/post_pin", JSONObject().apply {
            put("post_id", postId)
            put("pinned", pinned)
        })
    }

    suspend fun postRevisions(postId: String): JSONObject =
        postAuthed("/v1/connect/post_revisions", JSONObject().put("post_id", postId))

    suspend fun postSearch(query: String, limit: Int = 30): JSONObject =
        postAuthed("/v1/connect/post_search", JSONObject().apply {
            put("q", query)
            put("limit", limit)
        })

    suspend fun commentCreate(
        postId: String,
        body: String,
        parentId: String? = null,
        foldEpoch: Long? = null,
    ): String {
        val v = postAuthed("/v1/connect/comment_create", JSONObject().apply {
            put("post_id", postId)
            put("body", body)
            put("parent_id", parentId ?: JSONObject.NULL)
            put("fold_epoch", foldEpoch ?: JSONObject.NULL)
        })
        return v.getString("comment_id")
    }

    suspend fun comments(postId: String): JSONObject =
        postAuthed("/v1/connect/comments", JSONObject().put("post_id", postId))

    suspend fun commentReact(commentId: String, kind: String = "❤️") {
        postAuthed("/v1/connect/comment_react", JSONObject().apply {
            put("comment_id", commentId)
            put("kind", kind)
        })
    }

    suspend fun commentUnreact(commentId: String) {
        postAuthed("/v1/connect/comment_unreact", JSONObject().put("comment_id", commentId))
    }

    /** {discoverable, auto_accept, comments_from}. */
    suspend fun settingsGet(): JSONObject = getAuthed("/v1/connect/settings_get")

    suspend fun settingsSet(discoverable: Boolean, autoAccept: Boolean, commentsFrom: String) {
        postAuthed("/v1/connect/settings_set", JSONObject().apply {
            put("discoverable", discoverable)
            put("auto_accept", autoAccept)
            put("comments_from", commentsFrom)
        })
    }

    suspend fun handleSet(handle: String) {
        postAuthed("/v1/connect/handle_set", JSONObject().put("handle", handle))
    }

    suspend fun react(postId: String, kind: String = "like") {
        postAuthed("/v1/connect/react", JSONObject().apply {
            put("post_id", postId)
            put("kind", kind)
        })
    }

    suspend fun unreact(postId: String) {
        postAuthed("/v1/connect/unreact", JSONObject().put("post_id", postId))
    }

    suspend fun report(
        subjectKind: String,
        subjectId: String,
        reason: String,
        reporterCopy: String? = null,
    ) {
        postAuthed("/v1/connect/report", JSONObject().apply {
            put("subject_kind", subjectKind)
            put("subject_id", subjectId)
            put("reason", reason)
            put("reporter_copy", reporterCopy ?: JSONObject.NULL)
        })
    }

    /** Fetch a single post by id (permalinks from alerts); audience rules apply. */
    suspend fun postGet(postId: String): JSONObject =
        postAuthed("/v1/connect/post_get", JSONObject().put("post_id", postId))

    // ---------------------------------------------------------------- folds
    // Server-side these are the "circles" endpoints (§6.4); user-facing name
    // is Folds. Empty values represent membership only. The server rejects
    // Fold content until client-side E2E key wrapping is implemented.

    /** Folds I own (with member map) + folds I'm in. */
    suspend fun circles(): JSONObject = getAuthed("/v1/connect/circles")

    suspend fun circleCreate(name: String, wrappedKeys: JSONObject): String {
        val v = postAuthed("/v1/connect/circle_create", JSONObject().apply {
            put("name", name)
            put("wrapped_keys", wrappedKeys)
        })
        return v.getString("circle_id")
    }

    /** Replace the full member set of an owned fold. */
    suspend fun circleSetKeys(
        circleId: String,
        expectedEpoch: Long,
        wrappedKeys: JSONObject,
    ): Long {
        return postAuthed("/v1/connect/circle_set_keys", JSONObject().apply {
            put("circle_id", circleId)
            put("expected_epoch", expectedEpoch)
            put("wrapped_keys", wrappedKeys)
        }).getLong("key_epoch")
    }

    suspend fun foldInvite(circleId: String, target: String, wrappedKey: JSONObject) {
        postAuthed("/v1/connect/fold_invite", JSONObject().apply {
            put("circle_id", circleId)
            put("target", target)
            put("wrapped_key", wrappedKey)
        })
    }

    suspend fun foldKeyDirectory(target: String): JSONObject =
        postAuthed("/v1/connect/fold_key_directory", JSONObject().put("target", target))

    suspend fun foldAccept(circleId: String) {
        postAuthed("/v1/connect/fold_accept", JSONObject().put("circle_id", circleId))
    }

    suspend fun foldDecline(circleId: String) {
        postAuthed("/v1/connect/fold_decline", JSONObject().put("circle_id", circleId))
    }

    suspend fun foldRemove(circleId: String, target: String) {
        postAuthed("/v1/connect/fold_remove", JSONObject().apply {
            put("circle_id", circleId)
            put("target", target)
        })
    }

    suspend fun foldLeave(circleId: String) {
        postAuthed("/v1/connect/fold_leave", JSONObject().put("circle_id", circleId))
    }

    /** Delete a fold AND all its posts (real deletion). */
    suspend fun circleDelete(circleId: String) {
        postAuthed("/v1/connect/circle_delete", JSONObject().put("circle_id", circleId))
    }

    suspend fun supportOpen(category: String, subject: String, body: String): String =
        postAuthed("/v1/connect/support_open", JSONObject().apply {
            put("category", category)
            put("subject", subject)
            put("body", body)
        }).getString("thread_id")

    suspend fun supportSend(threadId: String, body: String) {
        postAuthed("/v1/connect/support_send", JSONObject().apply {
            put("thread_id", threadId)
            put("body", body)
        })
    }

    suspend fun supportThreads(): JSONObject = getAuthed("/v1/connect/support_threads")

    // ----------------------------------------------- community stewardship

    suspend fun communityInviteCreate(count: Int = 1): List<String> {
        val value = postAuthed(
            "/v1/connect/invite_create",
            JSONObject().put("count", count),
        )
        val codes = value.optJSONArray("codes")
        return (0 until (codes?.length() ?: 0)).map { codes!!.getString(it) }
    }

    suspend fun communityInviteList(): JSONObject = getAuthed("/v1/connect/invite_list")

    suspend fun communityInviteRevoke(code: String) {
        postAuthed("/v1/connect/invite_revoke", JSONObject().put("code", code))
    }

    // --------------------------------------------------------------- admin
    // Founder-only. Regular accounts get "admin only".

    /** Mint invite codes; prod registration requires one per account. */
    suspend fun adminInviteCreate(count: Int): List<String> {
        val v = postAuthed("/v1/admin/invite_create", JSONObject().put("count", count))
        val arr = v.optJSONArray("codes")
        return (0 until (arr?.length() ?: 0)).map { arr!!.getString(it) }
    }

    suspend fun adminInviteList(): JSONObject =
        postAuthed("/v1/admin/invite_list", JSONObject())

    suspend fun adminInviteRevoke(code: String) {
        postAuthed("/v1/admin/invite_revoke", JSONObject().put("code", code))
    }

    // ------------------------------------------------------------ streaming

    /**
     * The push channel (§2.1): hello → {token} → authed, then tagged frames
     * (connect_notif, sync_hint, ...). onFrame runs on OkHttp's thread.
     */
    fun stream(onFrame: (JSONObject) -> Unit, onClosed: () -> Unit): WebSocket {
        val t = token ?: throw HiveException("not signed in")
        val url = base.replaceFirst("https://", "wss://") + "/v1/stream"
        val req = Request.Builder().url(url).build()
        return http.newWebSocket(req, object : WebSocketListener() {
            override fun onMessage(webSocket: WebSocket, text: String) {
                val v = runCatching { JSONObject(text) }.getOrNull() ?: return
                when (v.optString("type")) {
                    "hello" -> webSocket.send(JSONObject().put("token", t).toString())
                    "authed" -> {}
                    "error" -> webSocket.close(1000, null)
                    else -> onFrame(v)
                }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) =
                onClosed()

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = onClosed()
        })
    }
}

// -------------------------------------------------------- escrow sealing
//
// The recovery bundle format, shared by every Nexus client:
//   [16-byte salt][12-byte nonce][AES-256-GCM ct of the 32-byte identity seed]
// Key = Argon2id(password, salt, m=19456 KB, t=2, p=1) — the suite floor,
// identical to the desktop Vault. The server only ever sees ciphertext.

object Escrow {
    private const val SALT_LEN = 16
    private const val NONCE_LEN = 12

    private fun deriveKey(password: String, salt: ByteArray): ByteArray {
        val params = org.bouncycastle.crypto.params.Argon2Parameters.Builder(
            org.bouncycastle.crypto.params.Argon2Parameters.ARGON2_id
        )
            .withVersion(org.bouncycastle.crypto.params.Argon2Parameters.ARGON2_VERSION_13)
            .withMemoryAsKB(19456)
            .withIterations(2)
            .withParallelism(1)
            .withSalt(salt)
            .build()
        val gen = org.bouncycastle.crypto.generators.Argon2BytesGenerator()
        gen.init(params)
        val key = ByteArray(32)
        gen.generateBytes(password.toByteArray(Charsets.UTF_8), key)
        return key
    }

    fun seal(password: String, seed: ByteArray): ByteArray {
        val rng = SecureRandom()
        val salt = ByteArray(SALT_LEN).also { rng.nextBytes(it) }
        val nonce = ByteArray(NONCE_LEN).also { rng.nextBytes(it) }
        val cipher = javax.crypto.Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            javax.crypto.Cipher.ENCRYPT_MODE,
            javax.crypto.spec.SecretKeySpec(deriveKey(password, salt), "AES"),
            javax.crypto.spec.GCMParameterSpec(128, nonce),
        )
        return salt + nonce + cipher.doFinal(seed)
    }

    /** Throws on a wrong password (GCM tag mismatch). */
    fun open(password: String, blob: ByteArray): ByteArray {
        if (blob.size < SALT_LEN + NONCE_LEN + 16) throw HiveException("corrupt recovery data")
        val salt = blob.copyOfRange(0, SALT_LEN)
        val nonce = blob.copyOfRange(SALT_LEN, SALT_LEN + NONCE_LEN)
        val ct = blob.copyOfRange(SALT_LEN + NONCE_LEN, blob.size)
        val cipher = javax.crypto.Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            javax.crypto.Cipher.DECRYPT_MODE,
            javax.crypto.spec.SecretKeySpec(deriveKey(password, salt), "AES"),
            javax.crypto.spec.GCMParameterSpec(128, nonce),
        )
        return try {
            cipher.doFinal(ct)
        } catch (e: Exception) {
            throw HiveException("wrong password")
        }
    }
}
