package com.nexusway.notify

/**
 * Owns Nexus Notify's encrypted relay credential, persistent HIVE wake socket,
 * foreground-service lifecycle, and privacy-limited fallback alerts. It never
 * decrypts Connect messages; the signed Connect receiver owns detailed sync.
 */

import android.Manifest
import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.net.ConnectivityManager
import android.net.Network
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import java.util.concurrent.TimeUnit
import java.util.concurrent.ConcurrentHashMap
import java.util.UUID
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.currentCoroutineContext
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.json.JSONObject

private const val ACTION_CONFIGURE = "com.nexusway.notify.CONFIGURE"
private const val ACTION_CLEAR = "com.nexusway.notify.CLEAR"
private const val ACTION_ACKNOWLEDGE = "com.nexusway.notify.ACKNOWLEDGE"
private const val EXTRA_SERVER = "server"
private const val EXTRA_SERVER_PIN = "server_pin"
private const val EXTRA_TOKEN = "token"
private const val EXTRA_RELAY_ID = "relay_id"
private const val CONNECT_FRAME_ACTION = "com.nexusway.connect.NOTIFICATION_RELAY_FRAME"
private const val CONNECT_PACKAGE = "com.nexusway.connect"
private const val CONNECT_RECEIVER = "com.nexusway.connect.NotificationRelayReceiver"

private data class RelayConfig(val server: String, val serverPin: String, val token: String)

private class RelayStore(context: Context) {
    private val prefs = EncryptedSharedPreferences.create(
        context,
        "notification_relay",
        MasterKey.Builder(context).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build(),
        EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
        EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
    )

    fun save(intent: Intent): RelayConfig? {
        val server = intent.getStringExtra(EXTRA_SERVER)?.trimEnd('/') ?: return null
        val serverPin = intent.getStringExtra(EXTRA_SERVER_PIN) ?: return null
        val token = intent.getStringExtra(EXTRA_TOKEN) ?: return null
        prefs.edit()
            .putString(EXTRA_SERVER, server)
            .putString(EXTRA_SERVER_PIN, serverPin)
            .putString(EXTRA_TOKEN, token)
            .commit()
        return RelayConfig(server, serverPin, token)
    }

    fun load(): RelayConfig? {
        val server = prefs.getString(EXTRA_SERVER, null) ?: return null
        val serverPin = prefs.getString(EXTRA_SERVER_PIN, null) ?: return null
        val token = prefs.getString(EXTRA_TOKEN, null) ?: return null
        return RelayConfig(server, serverPin, token)
    }

    fun clear() {
        prefs.edit().clear().commit()
    }
}

class RelayPermissionActivity : Activity() {
    private var pendingConfig: Intent? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        pendingConfig = intent
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        } else {
            startRelay()
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        startRelay()
    }

    private fun startRelay() {
        val config = pendingConfig ?: intent
        ContextCompat.startForegroundService(
            this,
            Intent(config).setClass(this, NotificationRelayService::class.java),
        )
        finish()
    }
}

class NotificationRelayService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val client = OkHttpClient.Builder()
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .pingInterval(20, TimeUnit.SECONDS)
        .build()
    private var connectionJob: kotlinx.coroutines.Job? = null
    private var socket: WebSocket? = null
    private val pendingAlerts = ConcurrentHashMap<String, Job>()
    private val pendingFrames = ConcurrentHashMap<String, String>()
    private var generation = 0L
    private var connectionStatus = "Connecting to HIVE"
    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            scope.launch { restartConnection() }
        }
        override fun onLost(network: Network) { socket?.cancel() }
    }

    override fun onCreate() {
        super.onCreate()
        createChannels()
        startForeground(NOTIFICATION_ID, serviceNotification())
        getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(networkCallback)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_ACKNOWLEDGE) {
            intent.getStringExtra(EXTRA_RELAY_ID)?.let { relayId ->
                pendingAlerts.remove(relayId)?.cancel()
                pendingFrames.remove(relayId)?.let { text ->
                    val frame = JSONObject(text)
                    fallbackAlert(frame, relayId)?.let { alert ->
                        getSystemService(NotificationManager::class.java).cancel(alert.id)
                    }
                }
            }
        }
        val store = RelayStore(this)
        if (intent?.action == ACTION_CLEAR) {
            store.clear()
            stopSelf()
            return START_NOT_STICKY
        }
        val configChanged = intent?.action == ACTION_CONFIGURE
        val config = if (configChanged) store.save(intent) else store.load()
        if (config == null) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (!configChanged && connectionJob?.isActive == true) return START_STICKY
        restartConnection(config)
        return START_STICKY
    }

    @Synchronized
    private fun restartConnection(config: RelayConfig? = RelayStore(this).load()) {
        if (config == null) return
        connectionJob?.cancel()
        socket?.cancel()
        val owner = ++generation
        connectionJob = scope.launch { maintainConnection(config, owner) }
    }

    override fun onDestroy() {
        getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(networkCallback)
        socket?.cancel()
        scope.cancel()
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    private suspend fun maintainConnection(config: RelayConfig, owner: Long) {
        val retryDelay = java.util.concurrent.atomic.AtomicLong(2_000L)
        while (currentCoroutineContext().isActive && owner == generation) {
            updateConnectionStatus("Connecting to HIVE")
            val disconnected = CompletableDeferred<Unit>()
            val request = Request.Builder()
                .url(config.server.replaceFirst("https://", "wss://") + "/v1/notification/stream")
                .build()
            socket = client.newWebSocket(request, object : WebSocketListener() {
                override fun onMessage(webSocket: WebSocket, text: String) {
                    if (owner != generation) return
                    val frame = runCatching { JSONObject(text) }.getOrNull() ?: return
                    when (frame.optString("type")) {
                        "hello" -> {
                            if (frame.optString("server_pub") != config.serverPin) {
                                Log.e(TAG, "HIVE server pin mismatch")
                                webSocket.close(1008, "server pin mismatch")
                            } else {
                                webSocket.send(JSONObject().put("token", config.token).toString())
                            }
                        }
                        "authed" -> {
                            retryDelay.set(2_000L)
                            updateConnectionStatus("Connected to HIVE")
                            Log.i(TAG, "notification relay connected")
                            forwardFrame("{\"type\":\"relay_ready\"}", JSONObject().put("type", "relay_ready"))
                        }
                        "error" -> webSocket.close(1008, "relay auth failed")
                        else -> {
                            Log.i(TAG, "forwarding relay frame type=${frame.optString("type")}")
                            forwardFrame(text, frame)
                        }
                    }
                }

                override fun onFailure(webSocket: WebSocket, error: Throwable, response: Response?) {
                    Log.w(TAG, "notification relay disconnected", error)
                    disconnected.complete(Unit)
                }

                override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                    disconnected.complete(Unit)
                }

                override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                    webSocket.close(code, reason)
                }
            })
            disconnected.await()
            if (owner != generation) return
            socket = null
            updateConnectionStatus("Disconnected; retrying")
            delay(retryDelay.get())
            retryDelay.updateAndGet { (it * 2).coerceAtMost(60_000L) }
        }
    }

    private fun updateConnectionStatus(status: String) {
        connectionStatus = status
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, serviceNotification())
    }

    private fun forwardFrame(frameText: String, frame: JSONObject) {
        if (frame.optString("type") == "call_signal" && frame.optString("action") in setOf("hangup", "reject", "accept")) {
            val callId = frame.optString("call_id")
            pendingFrames.entries.filter { (_, text) ->
                runCatching { JSONObject(text).optString("call_id") == callId }.getOrDefault(false)
            }.forEach { (pendingId, _) ->
                pendingAlerts.remove(pendingId)?.cancel()
                pendingFrames.remove(pendingId)
            }
        }
        val relayId = UUID.randomUUID().toString()
        val alert = fallbackAlert(frame, relayId)?.let { fallback ->
            pendingFrames[relayId] = frameText
            scope.launch(start = CoroutineStart.LAZY) {
                delay(FALLBACK_DELAY_MS)
                pendingAlerts.remove(relayId)
                postFallback(fallback)
                delay(5 * 60_000L)
                pendingFrames.remove(relayId)
            }.also { pendingAlerts[relayId] = it }
        }
        sendBroadcast(Intent(CONNECT_FRAME_ACTION).apply {
            setClassName(CONNECT_PACKAGE, CONNECT_RECEIVER)
            putExtra("frame", frameText)
            putExtra(EXTRA_RELAY_ID, relayId)
        })
        alert?.start()
    }

    private fun createChannels() {
        getSystemService(NotificationManager::class.java).createNotificationChannels(listOf(
            NotificationChannel(
                CHANNEL_ID,
                "Nexus notification relay",
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "Keeps Connect ready after its recent-app card is closed"
                setShowBadge(false)
                setSound(null, null)
                enableVibration(false)
            },
            NotificationChannel(
                MESSAGES_CHANNEL_ID,
                "Encrypted messages",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply { description = "Fallback alerts when Connect is closed" },
            NotificationChannel(
                ALERTS_CHANNEL_ID,
                "Social alerts",
                NotificationManager.IMPORTANCE_DEFAULT,
            ).apply { description = "Fallback alerts when Connect is closed" },
            NotificationChannel(
                CALLS_CHANNEL_ID,
                "Secure calls",
                NotificationManager.IMPORTANCE_HIGH,
            ).apply { description = "Fallback alerts when Connect is closed" },
        ))
    }

    private fun fallbackAlert(frame: JSONObject, relayId: String): FallbackAlert? {
        val type = frame.optString("type")
        return when (type) {
            "wire_msg" -> FallbackAlert(
                MESSAGES_CHANNEL_ID,
                frame.optString("msg_id", relayId).hashCode(),
                "New encrypted message",
                "Open Nexus Connect to read it",
                OPEN_MESSAGES_ACTION,
            )
            "wire_request" -> FallbackAlert(
                MESSAGES_CHANNEL_ID,
                frame.optString("from", relayId).hashCode(),
                "New message invite",
                "Open Nexus Connect to respond",
                OPEN_MESSAGES_ACTION,
            )
            "wire_accepted" -> FallbackAlert(
                MESSAGES_CHANNEL_ID,
                relayId.hashCode(),
                "Message invite accepted",
                "Open Nexus Connect to start chatting",
                OPEN_MESSAGES_ACTION,
            )
            "connect_notif" -> {
                val from = frame.optJSONObject("from")
                val actor = from?.optString("display_name")
                    ?.takeIf { it.isNotBlank() }
                    ?: from?.optString("handle")?.takeIf { it.isNotBlank() }
                FallbackAlert(
                    ALERTS_CHANNEL_ID,
                    frame.optString("post_id", relayId).hashCode(),
                    socialTitle(frame.optString("kind")),
                    actor?.let { "$it on Nexus Connect" } ?: "New activity on Nexus Connect",
                    if (frame.optString("fold_id").isNotEmpty() ||
                        frame.optString("kind") in FOLD_NOTIFICATION_KINDS
                    ) {
                        OPEN_FOLD_ACTION
                    } else {
                        OPEN_ALERTS_ACTION
                    },
                    frame.optString("kind"),
                    frame.optString("post_id"),
                    frame.optString("fold_id"),
                )
            }
            "call_signal" -> if (frame.optString("action") == "invite") {
                val kind = if (frame.optString("kind") == "video") "video" else "voice"
                FallbackAlert(
                    CALLS_CHANNEL_ID,
                    frame.optString("call_id", relayId).hashCode(),
                    "Incoming secure $kind call",
                    "Open Nexus Connect to answer",
                    OPEN_CALL_ACTION,
                    callId = frame.optString("call_id"),
                    callFrame = frame.toString(),
                    expiresAt = frame.optLong("expires_at", System.currentTimeMillis() / 1_000 + 60),
                )
            } else {
                frame.optString("call_id").takeIf { it.isNotEmpty() }?.let { callId ->
                    val manager = getSystemService(NotificationManager::class.java)
                    val incomingId = callId.hashCode()
                    val wasRinging = manager.activeNotifications.any { it.id == incomingId }
                    manager.cancel(incomingId)
                    if (frame.optString("action") == "hangup" && wasRinging) {
                        val kind = if (frame.optString("kind") == "video") "video" else "voice"
                        FallbackAlert(
                            CALLS_CHANNEL_ID,
                            31 * incomingId + 17,
                            "Missed secure $kind call",
                            "Open Nexus Connect to call back",
                            OPEN_MESSAGES_ACTION,
                        )
                    } else null
                }
            }
            else -> null
        }
    }

    private fun socialTitle(kind: String): String = when (kind) {
        "follow_request" -> "New follow request"
        "follow_accepted" -> "Follow request accepted"
        "follow" -> "New follower"
        "mention" -> "You were mentioned"
        "comment" -> "New comment"
        "reaction" -> "New reaction"
        else -> "New Connect alert"
    }

    private fun postFallback(alert: FallbackAlert) {
        if (alert.expiresAt != 0L && alert.expiresAt <= System.currentTimeMillis() / 1_000) return
        if (Build.VERSION.SDK_INT >= 33 &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) return
        val openConnect = PendingIntent.getActivity(
            this,
            alert.id,
            Intent().setClassName(CONNECT_PACKAGE, "$CONNECT_PACKAGE.MainActivity").apply {
                action = alert.action
                if (alert.callId.isNotEmpty()) {
                    putExtra("call_id", alert.callId)
                    putExtra("call_frame", alert.callFrame)
                    putExtra("call_expires_at", alert.expiresAt)
                }
                if (alert.kind.isNotEmpty()) putExtra(ALERT_KIND_EXTRA, alert.kind)
                if (alert.subject.isNotEmpty()) putExtra(ALERT_SUBJECT_EXTRA, alert.subject)
                if (alert.foldId.isNotEmpty()) putExtra(FOLD_ID_EXTRA, alert.foldId)
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val priority = if (alert.channel == ALERTS_CHANNEL_ID) {
            NotificationCompat.PRIORITY_DEFAULT
        } else {
            NotificationCompat.PRIORITY_HIGH
        }
        getSystemService(NotificationManager::class.java).notify(
            alert.id,
            NotificationCompat.Builder(this, alert.channel)
                .setSmallIcon(android.R.drawable.stat_notify_chat)
                .setContentTitle(alert.title)
                .setContentText(alert.text)
                .setContentIntent(openConnect)
                .setCategory(
                    if (alert.channel == CALLS_CHANNEL_ID) NotificationCompat.CATEGORY_CALL
                    else NotificationCompat.CATEGORY_MESSAGE,
                )
                .setPriority(priority)
                .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
                .setAutoCancel(true)
                .build(),
        )
        Log.i(TAG, "posted fallback notification channel=${alert.channel}")
    }

    private fun serviceNotification(): android.app.Notification {
        val openConnect = PendingIntent.getActivity(
            this,
            0,
            Intent().setClassName(CONNECT_PACKAGE, "$CONNECT_PACKAGE.MainActivity"),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.stat_notify_sync_noanim)
            .setContentTitle("Nexus Notify")
            .setContentText(connectionStatus)
            .setContentIntent(openConnect)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setPriority(NotificationCompat.PRIORITY_MIN)
            .setVisibility(NotificationCompat.VISIBILITY_SECRET)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .build()
    }

    companion object {
        private const val TAG = "NexusNotify"
        private const val CHANNEL_ID = "nexus_notification_relay"
        private const val MESSAGES_CHANNEL_ID = "connect_fallback_messages"
        private const val ALERTS_CHANNEL_ID = "connect_fallback_alerts"
        private const val CALLS_CHANNEL_ID = "connect_fallback_calls"
        private const val NOTIFICATION_ID = 20_001
        private const val FALLBACK_DELAY_MS = 1_500L
        private const val OPEN_ALERTS_ACTION = "com.nexusway.connect.OPEN_ALERTS"
        private const val OPEN_MESSAGES_ACTION = "com.nexusway.connect.OPEN_MESSAGES"
        private const val OPEN_CALL_ACTION = "com.nexusway.connect.OPEN_CALL"
        private const val OPEN_FOLD_ACTION = "com.nexusway.connect.OPEN_FOLD"
        private const val ALERT_KIND_EXTRA = "alert_kind"
        private const val ALERT_SUBJECT_EXTRA = "alert_subject"
        private const val FOLD_ID_EXTRA = "fold_id"
        private val FOLD_NOTIFICATION_KINDS = setOf("fold_invite", "fold_joined", "fold_post")
    }
}

private data class FallbackAlert(
    val channel: String,
    val id: Int,
    val title: String,
    val text: String,
    val action: String,
    val kind: String = "",
    val subject: String = "",
    val foldId: String = "",
    val callId: String = "",
    val callFrame: String = "",
    val expiresAt: Long = 0L,
)

class RelayBootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Intent.ACTION_BOOT_COMPLETED && RelayStore(context).load() != null) {
            ContextCompat.startForegroundService(
                context,
                Intent(context, NotificationRelayService::class.java),
            )
        }
    }
}