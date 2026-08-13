package com.nexusway.connect

/**
 * Owns Connect's explicit, signature-protected commands to Nexus Notify and
 * the receiver that accepts relayed frames. The companion application, not
 * this file, owns the persistent background socket and fallback notifications.
 */

import android.Manifest
import android.content.BroadcastReceiver
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.util.Log
import androidx.core.content.ContextCompat
import org.json.JSONObject

object NexusNotifier {
    private const val NOTIFIER_PACKAGE = "com.nexusway.notify"
    private const val CONFIGURE_ACTION = "com.nexusway.notify.CONFIGURE"
    private const val CLEAR_ACTION = "com.nexusway.notify.CLEAR"
    private const val ACKNOWLEDGE_ACTION = "com.nexusway.notify.ACKNOWLEDGE"
    private const val NOTIFIER_SERVICE = "$NOTIFIER_PACKAGE.NotificationRelayService"
    private const val PERMISSION_ACTIVITY = "$NOTIFIER_PACKAGE.RelayPermissionActivity"

    suspend fun provision(context: Context, enrollment: Enrollment, client: HiveClient) {
        if (!isInstalled(context)) return
        val configure = Intent(CONFIGURE_ACTION).apply {
            putExtra("server", enrollment.server)
            putExtra("server_pin", enrollment.serverPin)
            putExtra("token", client.notificationRelayToken())
        }
        val needsPermission = Build.VERSION.SDK_INT >= 33 &&
            context.packageManager.checkPermission(
                Manifest.permission.POST_NOTIFICATIONS,
                NOTIFIER_PACKAGE,
            ) != PackageManager.PERMISSION_GRANTED
        if (needsPermission) {
            context.startActivity(configure.apply {
                component = ComponentName(NOTIFIER_PACKAGE, PERMISSION_ACTIVITY)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            })
        } else {
            ContextCompat.startForegroundService(
                context,
                configure.apply { component = ComponentName(NOTIFIER_PACKAGE, NOTIFIER_SERVICE) },
            )
        }
    }

    fun clear(context: Context) {
        if (!isInstalled(context)) return
        runCatching {
            ContextCompat.startForegroundService(
                context,
                Intent(CLEAR_ACTION).apply {
                    component = ComponentName(NOTIFIER_PACKAGE, NOTIFIER_SERVICE)
                },
            )
        }
    }

    /** Restart the companion from its stored relay credential without rotating the token. */
    fun ensureRunning(context: Context) {
        if (!isInstalled(context)) return
        runCatching {
            ContextCompat.startForegroundService(
                context,
                Intent().apply { component = ComponentName(NOTIFIER_PACKAGE, NOTIFIER_SERVICE) },
            )
        }.onFailure { error ->
            Log.w("NexusNotifier", "notification companion recovery failed", error)
        }
    }

    fun acknowledge(context: Context, relayId: String?) {
        if (relayId.isNullOrEmpty()) return
        runCatching {
            ContextCompat.startForegroundService(
                context,
                Intent(ACKNOWLEDGE_ACTION).apply {
                    component = ComponentName(NOTIFIER_PACKAGE, NOTIFIER_SERVICE)
                    putExtra("relay_id", relayId)
                },
            )
        }.onFailure { error ->
            Log.w("NexusNotifier", "notification relay acknowledgement failed", error)
        }
    }

    private fun isInstalled(context: Context): Boolean = try {
        context.packageManager.getApplicationInfo(NOTIFIER_PACKAGE, 0)
        true
    } catch (_: PackageManager.NameNotFoundException) {
        false
    }
}

class NotificationRelayReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val frame = intent.getStringExtra("frame")
            ?.let { runCatching { JSONObject(it) }.getOrNull() }
            ?: return
        NexusNotifier.acknowledge(context, intent.getStringExtra("relay_id"))
        Log.i("NexusNotifier", "received relay frame type=${frame.optString("type")}")
        if (frame.optString("type") == "wire_request") {
            val from = frame.optString("from")
            if (from.isNotEmpty()) {
                val store = Store(context)
                store.hiddenConversations = store.hiddenConversations - from
            }
        }
        if (frame.optString("type") == "call_signal") {
            val callId = frame.optString("call_id")
            val store = Store(context)
            if (frame.optString("action") == "invite") {
                store.savePendingCall(frame)
                CallRinger.start(context, callId)
                ConnectNotifications.postIncomingCall(context, frame)
            } else {
                val pendingCall = store.loadPendingCall()
                    ?.takeIf { it.optString("call_id") == callId }
                store.clearPendingCall(callId)
                CallRinger.stop(callId)
                ConnectNotifications.cancelIncomingCall(context, callId)
                if (frame.optString("action") == "hangup" && pendingCall != null) {
                    ConnectNotifications.postMissedCall(
                        context,
                        callId,
                        pendingCall.optString("kind", "voice"),
                    )
                }
            }
        } else {
            ConnectNotifications.syncNow(context, expedited = true)
        }
        HiveStreamEvents.tryEmit(frame)
    }
}