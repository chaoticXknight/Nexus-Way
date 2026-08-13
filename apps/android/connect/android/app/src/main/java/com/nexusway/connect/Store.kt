// Owns Connect's device-encrypted enrollment, session, local message, and UI
// bookkeeping. It does not call HIVE or render screens; HiveClient and the UI
// layers consume this persisted state.

package com.nexusway.connect

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import org.json.JSONArray
import org.json.JSONObject

class Enrollment(
    val server: String,
    val serverPin: String,
    val accountId: String,
    val handle: String,
    /** Null on linked devices — the identity key lives on the enrolling
     * device only (§1.3); this device holds just its device key. */
    val identity: Key?,
    val device: Key,
)

data class PostDraft(
    val body: String = "",
    val audience: String = "followers",
    val altText: String = "",
    val contentWarning: String = "",
)

data class MessageHistoryMerge(
    val messages: List<DirectMessage>,
    val deletedIds: Set<String>,
)

fun mergeMessageHistories(
    local: List<DirectMessage>,
    remote: List<DirectMessage>,
    localDeleted: Set<String>,
    remoteDeleted: Set<String>,
): MessageHistoryMerge {
    val tombstones = (localDeleted + remoteDeleted).toList().takeLast(2_000).toSet()
    fun statusRank(status: String): Int = when (status) {
        "read" -> 4
        "received" -> 3
        "delivered" -> 2
        "sent" -> 1
        else -> 0
    }
    val messages = (local + remote)
        .groupBy { it.id }
        .mapNotNull { (_, copies) -> copies.maxByOrNull { statusRank(it.status) } }
        .filterNot { it.id in tombstones }
        .sortedWith(compareBy<DirectMessage> { it.sent }.thenBy { it.id })
        .takeLast(1_000)
    return MessageHistoryMerge(messages, tombstones)
}

fun mergeConversationVisibilityStates(
    local: Map<String, Pair<Boolean, Long>>,
    remote: Map<String, Pair<Boolean, Long>>,
): Map<String, Pair<Boolean, Long>> {
    val result = local.toMutableMap()
    remote.forEach { (accountId, incoming) ->
        val current = result[accountId]
        if (current == null || incoming.second > current.second ||
            (incoming.second == current.second && incoming.first && !current.first)
        ) {
            result[accountId] = incoming
        }
    }
    return result
}

class Store(context: Context) {
    private val prefs: SharedPreferences

    init {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        prefs = EncryptedSharedPreferences.create(
            context,
            "hive_enrollment",
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    fun load(): Enrollment? {
        val server = prefs.getString("server", null) ?: return null
        val pin = prefs.getString("server_pin", null) ?: return null
        val accountId = prefs.getString("account_id", null) ?: return null
        val handle = prefs.getString("handle", null) ?: return null
        val idSeed = prefs.getString("identity_seed", null)
        val devSeed = prefs.getString("device_seed", null) ?: return null
        return Enrollment(
            server, pin, accountId, handle,
            idSeed?.let { Key(unb64(it)) }, Key(unb64(devSeed)),
        )
    }

    fun save(e: Enrollment) {
        prefs.edit()
            .putString("server", e.server)
            .putString("server_pin", e.serverPin)
            .putString("account_id", e.accountId)
            .putString("handle", e.handle)
            .putString("identity_seed", e.identity?.let { b64(it.seed) })
            .putString("device_seed", b64(e.device.seed))
            .apply()
    }

    var sessionToken: String?
        get() = prefs.getString("session_token", null)
        set(v) {
            prefs.edit().putString("session_token", v).apply()
        }

    var themeMode: String
        get() = prefs.getString("theme_mode", "dark") ?: "dark"
        set(v) {
            prefs.edit().putString("theme_mode", v).apply()
        }

    var autoAcceptMessageInvites: Boolean
        get() = prefs.getBoolean("auto_accept_message_invites", false)
        set(v) {
            prefs.edit().putBoolean("auto_accept_message_invites", v).apply()
        }

    /** Automatic in-app updates: download, verify, and install new releases
     *  without a manual tap (silent after Connect is installer of record). */
    var autoUpdate: Boolean
        get() = prefs.getBoolean("auto_update", true)
        set(v) {
            prefs.edit().putBoolean("auto_update", v).apply()
        }

    fun loadPostDraft(): PostDraft = runCatching {
        val value = JSONObject(prefs.getString("post_draft", "{}") ?: "{}")
        PostDraft(
            body = value.optString("body"),
            audience = value.optString("audience", "followers"),
            altText = value.optString("alt_text"),
            contentWarning = value.optString("content_warning"),
        )
    }.getOrDefault(PostDraft())

    fun savePostDraft(draft: PostDraft) {
        prefs.edit().putString("post_draft", JSONObject().apply {
            put("body", draft.body)
            put("audience", draft.audience)
            put("alt_text", draft.altText)
            put("content_warning", draft.contentWarning)
        }.toString()).apply()
    }

    fun clearPostDraft() {
        prefs.edit().remove("post_draft").apply()
    }

    var notificationPermissionAsked: Boolean
        get() = prefs.getBoolean("notification_permission_asked", false)
        set(v) {
            prefs.edit().putBoolean("notification_permission_asked", v).apply()
        }

    fun savePendingCall(frame: JSONObject) {
        prefs.edit()
            .putString("pending_call_frame", frame.toString())
            .putLong("pending_call_saved_at", System.currentTimeMillis())
            .commit()
    }

    fun loadPendingCall(): JSONObject? {
        val savedAt = prefs.getLong("pending_call_saved_at", 0L)
        if (System.currentTimeMillis() - savedAt > 90_000L) {
            clearPendingCall()
            return null
        }
        return prefs.getString("pending_call_frame", null)?.let {
            runCatching { JSONObject(it) }.getOrNull()
        }
    }

    fun clearPendingCall(callId: String? = null) {
        if (callId != null && loadPendingCallId() != callId) return
        prefs.edit()
            .remove("pending_call_frame")
            .remove("pending_call_saved_at")
            .commit()
    }

    private fun loadPendingCallId(): String? = prefs.getString("pending_call_frame", null)?.let {
        runCatching { JSONObject(it).optString("call_id") }.getOrNull()
    }

    fun newNotificationIds(kind: String, currentIds: Collection<String>): Set<String> {
        synchronized(notificationLock) {
            val key = "notified_${kind}_ids"
            val primedKey = "notified_${kind}_primed"
            val previous = prefs.getStringSet(key, emptySet())?.toSet().orEmpty()
            val fresh = if (prefs.getBoolean(primedKey, false)) currentIds.toSet() - previous else emptySet()
            prefs.edit()
                .putStringSet(key, (previous + currentIds).toList().takeLast(500).toSet())
                .putBoolean(primedKey, true)
                .commit()
            return fresh
        }
    }

    fun retryNotificationIds(kind: String, ids: Collection<String>) {
        if (ids.isEmpty()) return
        synchronized(notificationLock) {
            val key = "notified_${kind}_ids"
            val previous = prefs.getStringSet(key, emptySet())?.toSet().orEmpty()
            prefs.edit().putStringSet(key, previous - ids.toSet()).commit()
        }
    }

    fun wireKey(): WireKey {
        prefs.getString("wire_seed", null)?.let { return WireKey(unb64(it)) }
        val key = WireKey.generate()
        prefs.edit().putString("wire_seed", b64(key.seed)).commit()
        return key
    }

    fun saveFoldKey(circleId: String, epoch: Long, key: ByteArray) {
        require(key.size == 32) { "invalid Fold key" }
        prefs.edit().putString("fold_key_${circleId}_$epoch", b64(key)).commit()
    }

    fun foldKey(circleId: String, epoch: Long): ByteArray? =
        prefs.getString("fold_key_${circleId}_$epoch", null)?.let {
            runCatching { unb64(it) }.getOrNull()?.takeIf { key -> key.size == 32 }
        }

    fun removeFoldKeys(circleId: String) {
        val prefix = "fold_key_${circleId}_"
        val editor = prefs.edit()
        prefs.all.keys.filter { it.startsWith(prefix) }.forEach(editor::remove)
        editor.commit()
    }

    companion object {
        private val notificationLock = Any()
    }

    fun loadDirectMessages(): List<DirectMessage> {
        val array = runCatching { JSONArray(prefs.getString("wire_history", "[]")) }
            .getOrElse { return emptyList() }
        return (0 until array.length()).mapNotNull { index ->
            runCatching {
                val value = array.getJSONObject(index)
                DirectMessage(
                    id = value.getString("id"),
                    peerAccount = value.optString("peer_account"),
                    peerHandle = value.getString("peer_handle"),
                    body = value.getString("body"),
                    sent = value.getLong("sent"),
                    mine = value.getBoolean("mine"),
                    status = value.optString("status", if (value.getBoolean("mine")) "sent" else "received"),
                    attachment = value.optJSONObject("attachment")?.let { attachment ->
                        MessageAttachment(
                            blobId = attachment.getString("blob_id"),
                            key = attachment.getString("key"),
                            nonce = attachment.getString("nonce"),
                            mime = attachment.optString("mime", "image/jpeg"),
                        )
                    },
                )
            }.getOrNull()
        }
    }

    fun saveDirectMessages(messages: List<DirectMessage>) {
        prefs.edit().putString("wire_history", encodeDirectMessages(messages).toString()).apply()
    }

    private fun encodeDirectMessages(messages: List<DirectMessage>): JSONArray = JSONArray().apply {
        messages.takeLast(1_000).forEach { message ->
            put(JSONObject().apply {
                put("id", message.id)
                put("peer_account", message.peerAccount)
                put("peer_handle", message.peerHandle)
                put("body", message.body)
                put("sent", message.sent)
                put("mine", message.mine)
                put("status", message.status)
                message.attachment?.let { attachment ->
                    put("attachment", JSONObject().apply {
                        put("blob_id", attachment.blobId)
                        put("key", attachment.key)
                        put("nonce", attachment.nonce)
                        put("mime", attachment.mime)
                    })
                }
            })
        }
    }

    fun exportMessageHistory(): ByteArray {
        val messages = loadDirectMessages().toMutableList()
        while (true) {
            val bytes = JSONObject().apply {
                put("v", 1)
                put("messages", encodeDirectMessages(messages))
                put("deleted_ids", JSONArray(deletedMessageIds.toList()))
                put("conversation_visibility", encodeConversationVisibility())
                put("exported_at", System.currentTimeMillis() / 1000)
            }.toString().toByteArray()
            if (bytes.size <= 600 * 1024 || messages.isEmpty()) return bytes
            messages.removeAt(0)
        }
    }

    fun mergeMessageHistory(snapshot: ByteArray): Pair<List<DirectMessage>, Long> {
        val value = JSONObject(String(snapshot))
        require(value.optInt("v") == 1) { "unsupported message history version" }
        val remoteMessages = value.optJSONArray("messages") ?: JSONArray()
        val remoteDeleted = value.optJSONArray("deleted_ids") ?: JSONArray()
        val remoteVisibility = value.optJSONObject("conversation_visibility") ?: JSONObject()
        val parsedRemote = (0 until remoteMessages.length()).mapNotNull { index ->
            runCatching {
                val message = remoteMessages.getJSONObject(index)
                DirectMessage(
                    id = message.getString("id"),
                    peerAccount = message.optString("peer_account"),
                    peerHandle = message.getString("peer_handle"),
                    body = message.getString("body"),
                    sent = message.getLong("sent"),
                    mine = message.getBoolean("mine"),
                    status = message.optString(
                        "status",
                        if (message.getBoolean("mine")) "sent" else "received",
                    ),
                    attachment = message.optJSONObject("attachment")?.let { attachment ->
                        MessageAttachment(
                            blobId = attachment.getString("blob_id"),
                            key = attachment.getString("key"),
                            nonce = attachment.getString("nonce"),
                            mime = attachment.optString("mime", "image/jpeg"),
                        )
                    },
                )
            }.getOrNull()
        }
        val remoteTombstones = (0 until remoteDeleted.length()).mapNotNull {
            remoteDeleted.optString(it).takeIf { id -> id.length == 32 }
        }.toSet()
        val merged = mergeMessageHistories(
            loadDirectMessages(),
            parsedRemote,
            deletedMessageIds,
            remoteTombstones,
        )
        deletedMessageIds = merged.deletedIds
        saveDirectMessages(merged.messages)
        mergeConversationVisibility(remoteVisibility)
        val syncedThrough = merged.messages.maxOfOrNull { it.sent } ?: 0L
        lastMessageHistorySync = System.currentTimeMillis() / 1000
        return merged.messages to syncedThrough
    }

    var lastMessageHistorySync: Long
        get() = prefs.getLong("wire_history_last_sync", 0L)
        set(value) { prefs.edit().putLong("wire_history_last_sync", value).apply() }

    var deletedMessageIds: Set<String>
        get() = prefs.getStringSet("wire_deleted_ids", emptySet())?.toSet().orEmpty()
        set(value) {
            prefs.edit().putStringSet("wire_deleted_ids", value.toList().takeLast(2_000).toSet()).apply()
        }

    private fun loadConversationVisibility(): MutableMap<String, Pair<Boolean, Long>> {
        val result = mutableMapOf<String, Pair<Boolean, Long>>()
        val stored = runCatching {
            JSONObject(prefs.getString("wire_conversation_visibility", "{}") ?: "{}")
        }.getOrElse { JSONObject() }
        stored.keys().forEach { accountId ->
            val state = stored.optJSONObject(accountId) ?: return@forEach
            result[accountId] = state.optBoolean("hidden") to state.optLong("updated")
        }
        prefs.getStringSet("wire_hidden_conversations", emptySet()).orEmpty().forEach { accountId ->
            result.putIfAbsent(accountId, true to 1L)
        }
        return result
    }

    private fun saveConversationVisibility(states: Map<String, Pair<Boolean, Long>>) {
        val value = JSONObject()
        states.forEach { (accountId, state) ->
            value.put(accountId, JSONObject().apply {
                put("hidden", state.first)
                put("updated", state.second)
            })
        }
        prefs.edit()
            .putString("wire_conversation_visibility", value.toString())
            .putStringSet(
                "wire_hidden_conversations",
                states.filterValues { it.first }.keys,
            )
            .apply()
    }

    private fun encodeConversationVisibility(): JSONObject = JSONObject().apply {
        loadConversationVisibility().forEach { (accountId, state) ->
            put(accountId, JSONObject().apply {
                put("hidden", state.first)
                put("updated", state.second)
            })
        }
    }

    private fun mergeConversationVisibility(remote: JSONObject) {
        val incoming = mutableMapOf<String, Pair<Boolean, Long>>()
        remote.keys().forEach { accountId ->
            val value = remote.optJSONObject(accountId) ?: return@forEach
            incoming[accountId] = value.optBoolean("hidden") to value.optLong("updated")
        }
        saveConversationVisibility(
            mergeConversationVisibilityStates(loadConversationVisibility(), incoming),
        )
    }

    var hiddenConversations: Set<String>
        get() = loadConversationVisibility().filterValues { it.first }.keys
        set(value) {
            val states = loadConversationVisibility()
            val now = System.currentTimeMillis()
            (states.keys + value).forEach { accountId ->
                val hidden = accountId in value
                if (states[accountId]?.first != hidden) states[accountId] = hidden to now
            }
            saveConversationVisibility(states)
        }

    /** Keep the cached handle in sync after a server-side handle change. */
    fun updateHandle(handle: String) {
        prefs.edit().putString("handle", handle).apply()
    }

    fun wipe() {
        val theme = themeMode
        prefs.edit().clear().putString("theme_mode", theme).apply()
    }
}
