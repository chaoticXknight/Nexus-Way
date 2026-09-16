package com.nexusway.connect

/**
 * Owns Android notification channels, verified inbox parsing, and WorkManager
 * catch-up. It does not keep a socket alive; Nexus Notify owns background wake
 * delivery and hands frames to Connect through NotificationRelayReceiver.
 */

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingPeriodicWorkPolicy
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.OutOfQuotaPolicy
import androidx.work.PeriodicWorkRequestBuilder
import androidx.work.WorkManager
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject

const val OPEN_ALERTS_ACTION = "com.nexusway.connect.OPEN_ALERTS"
const val OPEN_MESSAGES_ACTION = "com.nexusway.connect.OPEN_MESSAGES"
const val OPEN_CALL_ACTION = "com.nexusway.connect.OPEN_CALL"
const val ANSWER_CALL_ACTION = "com.nexusway.connect.ANSWER_CALL" // Distinguish an explicit answer from merely opening the call screen.
const val DECLINE_CALL_ACTION = "com.nexusway.connect.DECLINE_CALL" // Bind notification rejection to its original call ID.
const val OPEN_FOLD_ACTION = "com.nexusway.connect.OPEN_FOLD"
const val CALL_ID_EXTRA = "call_id"
const val CALL_KIND_EXTRA = "call_kind"
const val ALERT_KIND_EXTRA = "alert_kind"
const val ALERT_SUBJECT_EXTRA = "alert_subject"
const val FOLD_ID_EXTRA = "fold_id"

data class VerifiedInbox(
    val messages: List<DirectMessage>,
    val verifiedIds: List<String>,
    val deletions: List<VerifiedDeletion>,
)

data class VerifiedDeletion(val targetId: String, val senderAccount: String)

fun verifyInbox(accountId: String, wire: WireKey, array: JSONArray): VerifiedInbox {
    val messages = mutableListOf<DirectMessage>()
    val verifiedIds = mutableListOf<String>()
    val deletions = mutableListOf<VerifiedDeletion>()
    for (index in 0 until array.length()) {
        val row = array.getJSONObject(index)
        val id = row.optString("msg_id")
        val payload = runCatching {
            JSONObject(String(WireCrypto.decrypt(wire, id, row.getString("envelope"))))
        }.getOrNull() ?: continue
        val senderAccount = payload.optString("sender_account")
        val recipientAccount = payload.optString("recipient_account")
        val device = payload.optJSONObject("sender_device") ?: continue
        val identityPub = payload.optString("sender_identity_pub")
        if (senderAccount != row.optString("sender_hint") ||
            !WireCrypto.validateDevice(senderAccount, identityPub, device)
        ) continue
        val kind = payload.optString("kind", "message")
        val attachment = payload.optJSONObject("attachment")?.let {
            MessageAttachment(
                blobId = it.optString("blob_id"),
                key = it.optString("key"),
                nonce = it.optString("nonce"),
                mime = it.optString("mime"),
            )
        }
        val signed = if (kind == "delete") {
            WireCrypto.signedDeletion(
                id, payload.optString("target_id"), senderAccount, recipientAccount,
                payload.optLong("deleted_at"),
            )
        } else if (payload.optInt("v") == 2 && attachment != null) {
            WireCrypto.signedMessageV2(
                id, senderAccount, recipientAccount, payload.optLong("sent"),
                payload.optString("body"), attachment,
            )
        } else {
            WireCrypto.signedMessage(
                id, senderAccount, recipientAccount, payload.optLong("sent"), payload.optString("body"),
            )
        }
        if (!Key.verify(
                runCatching { unb64(device.getString("device_pub")) }.getOrNull() ?: continue,
                signed.toByteArray(),
                runCatching { unb64(payload.getString("signature")) }.getOrNull() ?: continue,
            )
        ) continue
        if (kind == "delete") {
            val targetId = payload.optString("target_id")
            if (targetId.length != 32) continue
            deletions += VerifiedDeletion(targetId, senderAccount)
        } else {
            val mine = senderAccount == accountId
            messages += DirectMessage(
                id = id,
                peerAccount = if (mine) recipientAccount else senderAccount,
                peerHandle = if (mine) payload.optString("recipient_handle")
                    else payload.optString("sender_handle"),
                body = payload.optString("body"),
                sent = payload.optLong("sent"),
                mine = mine,
                status = if (mine) "sent" else "received",
                attachment = attachment,
            )
        }
        verifiedIds += id
    }
    return VerifiedInbox(messages, verifiedIds, deletions)
}

object ConnectNotifications {
    private val deliveryLock = Any()

    fun deliverPendingMessages(context: Context): Boolean = synchronized(deliveryLock) {
        val store = Store(context)
        var delivered = true
        store.pendingMessageAlerts().forEach { message ->
            if (postMessage(context, message)) store.completeMessageAlert(message.id)
            else delivered = false
        }
        delivered
    }

    private const val ALERTS_CHANNEL = "social_alerts"
    private const val MESSAGES_CHANNEL = "encrypted_messages"
    private const val CALLS_CHANNEL = "secure_calls_v3" // Android cannot change sound on the existing, app-created silent channel.
    private val deliveredChannels = setOf(ALERTS_CHANNEL, MESSAGES_CHANNEL, CALLS_CHANNEL)

    fun createChannels(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannels(listOf(
            NotificationChannel(
                ALERTS_CHANNEL,
                "Social alerts",
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply { description = "Follows, mentions, reactions, and comments" },
            NotificationChannel(
                MESSAGES_CHANNEL,
                "Encrypted messages",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply { description = "Message invites and end-to-end encrypted messages" },
            NotificationChannel(
                CALLS_CHANNEL,
                "Secure calls",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply {
                description = "Incoming encrypted voice and video calls"
                setSound(android.media.RingtoneManager.getDefaultUri(android.media.RingtoneManager.TYPE_RINGTONE), // Let Android ring even when Connect is backgrounded.
                    android.media.AudioAttributes.Builder().setUsage(android.media.AudioAttributes.USAGE_NOTIFICATION_RINGTONE).build()) // Respect ringtone volume and Do Not Disturb.
                enableVibration(true) // Incoming calls also alert phones in vibrate mode.
            },
        ))
    }

    fun schedule(context: Context) {
        val request = PeriodicWorkRequestBuilder<NotificationSyncWorker>(15, TimeUnit.MINUTES)
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .build()
        WorkManager.getInstance(context).enqueueUniquePeriodicWork(
            "connect-notification-sync",
            ExistingPeriodicWorkPolicy.UPDATE,
            request,
        )
    }

    fun syncNow(context: Context, expedited: Boolean = false, relayId: String? = null) {
        val request = OneTimeWorkRequestBuilder<NotificationSyncWorker>()
            .setInputData(androidx.work.workDataOf("relay_id" to relayId))
            .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
            .apply {
                if (expedited) setExpedited(OutOfQuotaPolicy.RUN_AS_NON_EXPEDITED_WORK_REQUEST)
            }
            .build()
        WorkManager.getInstance(context).enqueueUniqueWork(
            "connect-notification-sync-now",
            ExistingWorkPolicy.APPEND_OR_REPLACE,
            request,
        )
    }

    fun clearDelivered(context: Context) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.activeNotifications
            .filter { it.notification.channelId in deliveredChannels }
            .filterNot { it.notification.channelId == CALLS_CHANNEL && it.id == CallSession.current?.state?.takeIf { state -> state.phase == CallPhase.INCOMING }?.callId?.hashCode() } // Reading messages must not silence an unanswered call.
            .forEach { manager.cancel(it.tag, it.id) }
    }

    fun postAlert(context: Context, alert: Alert): Boolean {
        val title = when (alert.kind) {
            "follow_request" -> "New follow request"
            "follow_accepted" -> "Follow request accepted"
            "follow" -> "New follower"
            "mention" -> "You were mentioned"
            "comment" -> "New comment"
            "reaction" -> "New reaction"
            "post" -> "New post"
            "fold_invite" -> "Fold invitation"
            "fold_joined" -> "Someone joined your Fold"
            "fold_post" -> "New Fold post"
            "support_reply" -> "Support replied"
            "message" -> "New encrypted message"
            "message_request" -> "New message request"
            "system" -> alert.title.ifEmpty { "Connect announcement" }
            else -> "New Connect alert"
        }
        val body = if (alert.kind == "system") {
            alert.body.ifEmpty { "Open Connect to read the announcement" }
        } else {
            "${alert.from.label} on Nexus Connect"
        }
        return post(
            context, ALERTS_CHANNEL, alert.id.hashCode(), title,
            body,
            if (alert.foldId.isNotEmpty() ||
                alert.kind in setOf("fold_invite", "fold_joined", "fold_post")
            ) {
                OPEN_FOLD_ACTION
            } else {
                OPEN_ALERTS_ACTION
            },
            "connect-alerts",
            alertKind = alert.kind,
            alertSubject = alert.subjectId,
            foldId = alert.foldId,
        )
    }

    fun postMessage(context: Context, message: DirectMessage): Boolean {
        if (message.mine) return true
        return post(
            context, MESSAGES_CHANNEL, message.id.hashCode(), "@${message.peerHandle}",
            message.body.ifBlank {
                when {
                    message.attachment?.mime?.startsWith("audio/") == true -> "Sent a voice note"
                    message.attachment?.mime?.startsWith("video/") == true -> "Sent a video"
                    message.attachment != null -> "Sent a photo"
                    else -> "New message"
                }
            },
            OPEN_MESSAGES_ACTION, "connect-messages",
        )
    }

    fun postMessageInvite(context: Context, accountId: String, label: String): Boolean {
        return post(
            context, MESSAGES_CHANNEL, accountId.hashCode(), "New message invite",
            "$label wants to start an encrypted conversation", OPEN_MESSAGES_ACTION,
            "connect-messages",
        )
    }

    fun postIncomingCall(context: Context, frame: JSONObject): Boolean {
        val kind = if (frame.optString("kind") == "video") "video" else "voice"
        val callId = frame.optString("call_id") // Tie display and all actions to the verified invite.
        val remaining = incomingCallRemainingMillis(frame.optLong("expires_at", Long.MAX_VALUE), System.currentTimeMillis()) // Retry only for the original invite lifetime.
        if (remaining == 0L) return true // Expired calls must not wake or ring the phone.
        val state = CallSession.current?.state ?: return false // Never display unverified caller-supplied identities.
        if (state.callId != callId || state.phase != CallPhase.INCOMING) return true // Duplicate delivery must not resurrect an answered call.
        val manager = NotificationManagerCompat.from(context) // Respect the user's notification permission and channel choices.
        if (!manager.areNotificationsEnabled()) return false
        if (Build.VERSION.SDK_INT >= 33 && ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return false // Permission can be revoked independently of a channel's importance.
        if (context.getSystemService(NotificationManager::class.java).getNotificationChannel(CALLS_CHANNEL)?.importance == NotificationManager.IMPORTANCE_NONE) return false
        val existing = context.getSystemService(NotificationManager::class.java).activeNotifications // Redelivered invites should leave an already-ringing notification alone.
            .firstOrNull { it.id == callId.hashCode() && it.notification.channelId == CALLS_CHANNEL }?.notification // Match only this call's incoming channel.
        if (existing != null) return true // Android 12 can silence any update to its looping ringtone; retain the initial verified identity until the call ends.
        fun action(name: String): PendingIntent = PendingIntent.getActivity(context, 0, // Intent data prevents collisions between different calls and actions.
            Intent(context, IncomingCallActivity::class.java).setAction(name) // Open only the call controls above the lock screen.
                .setData(android.net.Uri.Builder().scheme("nexus-call").authority(name).appendPath(callId).build()) // Keep notification identities stable on redelivery.
                .putExtra(CALL_ID_EXTRA, callId), PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE) // Other apps cannot rewrite the action payload.
        val open = action(OPEN_CALL_ACTION) // Full-screen presentation never automatically answers.
        val person = androidx.core.app.Person.Builder().setName(state.peerLabel).setKey(state.peerAccount).setImportant(true).build() // Android's call template displays the verified caller label.
        val notification = NotificationCompat.Builder(context, CALLS_CHANNEL) // Use Android's native incoming-call template.
            .setSmallIcon(R.drawable.ic_connect_notification)
            .setContentTitle(state.peerLabel) // Show who is calling in the notification and lock-screen template.
            .setContentText("Incoming secure $kind call")
            .setStyle(NotificationCompat.CallStyle.forIncomingCall(person, action(DECLINE_CALL_ACTION), action(ANSWER_CALL_ACTION)).setIsVideo(kind == "video")) // Provide native Answer and Decline controls.
            .setContentIntent(open)
            .setFullScreenIntent(open, true) // Android presents the call screen when locked, or a heads-up call when unlocked.
            .setPriority(NotificationCompat.PRIORITY_MAX) // Support heads-up presentation on older Android versions.
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC) // Caller identity is intentionally visible on the incoming-call lock screen.
            .setOngoing(true) // FLAG_ONLY_ALERT_ONCE would stop an active FLAG_INSISTENT ringtone when the caller name updates.
            .setTimeoutAfter(remaining) // Android stops the alert at invite expiry even if the application process is killed.
            .build().apply { flags = flags or android.app.Notification.FLAG_INSISTENT } // Repeat the system ringtone until answer, decline, or timeout.
        return runCatching { manager.notify(callId.hashCode(), notification); true }.getOrDefault(false) // Leave failed deliveries unacknowledged for retry.
    }

    fun cancelIncomingCall(context: Context, callId: String) {
        if (callId.isNotEmpty()) {
            NotificationManagerCompat.from(context).cancel(callId.hashCode())
        }
    }

    fun postMissedCall(context: Context, callId: String, kind: String): Boolean {
        if (callId.isEmpty()) return false
        val normalizedKind = if (kind == "video") "video" else "voice"
        return post(
            context,
            CALLS_CHANNEL,
            missedCallNotificationId(callId),
            "Missed secure $normalizedKind call",
            "Open Nexus Connect to call back",
            OPEN_MESSAGES_ACTION,
            "connect-calls",
        )
    }

    private fun missedCallNotificationId(callId: String): Int = 31 * callId.hashCode() + 17

    private fun post(
        context: Context,
        channel: String,
        id: Int,
        title: String,
        text: String,
        action: String,
        group: String,
        callId: String = "",
        callKind: String = "",
        alertKind: String = "",
        alertSubject: String = "",
        foldId: String = "",
    ): Boolean {
        val manager = NotificationManagerCompat.from(context)
        if (!manager.areNotificationsEnabled()) return false
        if (context.getSystemService(NotificationManager::class.java)
            .getNotificationChannel(channel)?.importance == NotificationManager.IMPORTANCE_NONE
        ) return false
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) return false
        val intent = Intent(context, MainActivity::class.java).apply {
            this.action = action
            if (callId.isNotEmpty()) putExtra(CALL_ID_EXTRA, callId)
            if (callKind.isNotEmpty()) putExtra(CALL_KIND_EXTRA, callKind)
            if (alertKind.isNotEmpty()) putExtra(ALERT_KIND_EXTRA, alertKind)
            if (alertSubject.isNotEmpty()) putExtra(ALERT_SUBJECT_EXTRA, alertSubject)
            if (foldId.isNotEmpty()) putExtra(FOLD_ID_EXTRA, foldId)
            flags = Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP
        }
        val pending = PendingIntent.getActivity(
            context, 31 * action.hashCode() + id, intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val publicVersion = NotificationCompat.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_connect_notification)
            .setContentTitle("Nexus Connect")
            .setContentText("New activity")
            .build()
        val notification = NotificationCompat.Builder(context, channel)
            .setSmallIcon(R.drawable.ic_connect_notification)
            .setContentTitle(title)
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setContentIntent(pending)
            .setAutoCancel(true)
            .setGroup(group)
            .setCategory(when (channel) {
                MESSAGES_CHANNEL -> NotificationCompat.CATEGORY_MESSAGE
                CALLS_CHANNEL -> NotificationCompat.CATEGORY_CALL
                else -> NotificationCompat.CATEGORY_SOCIAL
            })
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setPublicVersion(publicVersion)
            .build()
        return runCatching {
            NotificationManagerCompat.from(context).notify(id, notification)
            true
        }.getOrDefault(false)
    }
}

class NotificationSyncWorker(
    context: Context,
    params: androidx.work.WorkerParameters,
) : CoroutineWorker(context, params) {
    companion object {
        private val syncLock = kotlinx.coroutines.sync.Mutex()
    }

    override suspend fun doWork(): Result {
        syncLock.lock()
        return try { sync() } finally { syncLock.unlock() }
    }

    private suspend fun sync(): Result {
        val store = Store(applicationContext)
        val enrollment = store.load() ?: return Result.success()
        return try {
            val client = SessionManager.client(applicationContext, enrollment)
            var alertsDelivered = NotificationManagerCompat.from(applicationContext).areNotificationsEnabled()

            val conversations = client.wireConversations().optJSONArray("conversations") ?: JSONArray()
            val incoming = (0 until conversations.length()).map { conversations.getJSONObject(it) }
                .filter { it.optString("direction") == "incoming" }
            if (store.autoAcceptMessageInvites) {
                incoming.forEach { client.wireRespond(it.getString("account_id"), true) }
                store.hiddenConversations = store.hiddenConversations -
                    incoming.map { it.getString("account_id") }.toSet()
            } else {
                val freshInvites = store.newNotificationIds(
                    "invite",
                    incoming.map { it.getString("account_id") },
                )
                val failedInvites = incoming.filter { it.getString("account_id") in freshInvites }
                    .filterNot {
                    ConnectNotifications.postMessageInvite(
                        applicationContext,
                        it.getString("account_id"),
                        Author.of(it).label,
                    )
                }
                store.retryNotificationIds("invite", failedInvites.map { it.getString("account_id") })
                alertsDelivered = alertsDelivered && failedInvites.isEmpty()
            }

            val inbox = client.wireInbox().optJSONArray("messages") ?: JSONArray()
            val verified = verifyInbox(enrollment.accountId, store.wireKey(), inbox)
            val cachedMessages = (store.loadDirectMessages() + verified.messages).associateBy { it.id }
            val validDeletions = verified.deletions.filter { deletion ->
                cachedMessages[deletion.targetId]?.let { target ->
                    deletion.senderAccount == if (target.mine) enrollment.accountId else target.peerAccount
                } == true
            }
            val deletedIds = store.deletedMessageIds + validDeletions.map { it.targetId }
            store.deletedMessageIds = deletedIds
            val allMessages = cachedMessages.values
                .associateBy { it.id }.values.sortedBy { it.sent }
                .filterNot { it.id in deletedIds }
            store.saveDirectMessages(allMessages)
            store.queueMessageNotifications(verified.messages)
            alertsDelivered = ConnectNotifications.deliverPendingMessages(applicationContext) && alertsDelivered
            client.wireAck(verified.verifiedIds)
            if (inbox.length() == 500) ConnectNotifications.syncNow(applicationContext)

            val alerts = notificationCatchup(store.notificationWatermark) { cursor -> client.notifications(cursor, 100) }
            val parsedAlerts = alerts.map { value ->
                Alert(
                    value.getString("id"), value.optString("kind"),
                    Author.of(value.getJSONObject("from")), value.optString("subject_id"),
                    value.optLong("created"), value.optBoolean("seen"),
                    title = value.optString("title"),
                    body = value.optString("body"),
                    announceKind = value.optString("announce_kind"),
                    foldId = value.optString("fold_id"),
                )
            }
            // Only unseen alerts are notification candidates; anything the
            // user already viewed in-app must stay silent.
            val unseenAlerts = parsedAlerts.filter { !it.seen }
            val freshAlertIds = store.newNotificationIds("alert", unseenAlerts.map { it.id })
            val failedAlerts = unseenAlerts.filter { it.id in freshAlertIds }
                .filterNot { ConnectNotifications.postAlert(applicationContext, it) }
            store.retryNotificationIds("alert", failedAlerts.map { it.id })
            if (alertsDelivered && failedAlerts.isEmpty()) {
                parsedAlerts.firstOrNull()?.let { store.notificationWatermark = it.id }
                NexusNotifier.acknowledge(applicationContext, inputData.getString("relay_id"))
            }
            Result.success()
        } catch (error: Exception) {
            if (error is kotlinx.coroutines.CancellationException) throw error
            android.util.Log.w("NotificationSync", "notification sync failed; retrying", error)
            Result.retry()
        }
    }
}