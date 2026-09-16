package com.nexusway.connect

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.core.content.ContextCompat

class CallService : Service() {
    private var wakeLock: PowerManager.WakeLock? = null
    private val handler = android.os.Handler(android.os.Looper.getMainLooper())
    private val renewWakeLock = object : Runnable {
        override fun run() {
            wakeLock?.acquire(2 * 60 * 60 * 1_000L)
            handler.postDelayed(this, 30 * 60 * 1_000L)
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        android.util.Log.i("NexusCall", "call service action=${intent?.action} backgroundRestricted=${Build.VERSION.SDK_INT >= 28 && getSystemService(android.app.ActivityManager::class.java).isBackgroundRestricted} batteryExempt=${getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName)}") // Record Android power policy alongside call failures without logging private identities.
        if (intent?.action == "hangup" || intent == null) {
            CallSession.current?.hangup()
            stopSelf()
            return START_NOT_STICKY
        }
        val kind = intent.getStringExtra("kind") ?: "voice"
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel("ongoing_calls", "Active calls", NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(this, 0,
            Intent(this, MainActivity::class.java).setAction(OPEN_CALL_ACTION),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val hangup = PendingIntent.getService(this, 1,
            Intent(this, CallService::class.java).setAction("hangup"),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        val notification = NotificationCompat.Builder(this, "ongoing_calls")
            .setSmallIcon(R.drawable.ic_connect_notification)
            .setContentTitle("Nexus Connect call")
            .setContentText("Secure $kind call in progress")
            .setContentIntent(open)
            .setCategory(NotificationCompat.CATEGORY_CALL)
            .setVisibility(NotificationCompat.VISIBILITY_PRIVATE)
            .setOngoing(true).setOnlyAlertOnce(true)
            .addAction(android.R.drawable.ic_menu_close_clear_cancel, "End call", hangup)
            .build()
        val types = if (Build.VERSION.SDK_INT >= 30) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE or
                if (kind == "video") ServiceInfo.FOREGROUND_SERVICE_TYPE_CAMERA else 0
        } else 0
        ServiceCompat.startForeground(this, 20_002, notification, types)
        if (wakeLock == null) {
            wakeLock = getSystemService(PowerManager::class.java)
                .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "NexusConnect:Call").apply {
                    setReferenceCounted(false)
                    acquire(2 * 60 * 60 * 1_000L)
                }
            handler.postDelayed(renewWakeLock, 30 * 60 * 1_000L)
        }
        CallSession.serviceStarted()
        val manager = CallSession.manager()
        when (intent.action) {
            "start" -> if (manager.state.phase == CallPhase.IDLE) {
                manager.start(Author(intent.getStringExtra("peer") ?: "",
                    intent.getStringExtra("handle") ?: "", intent.getStringExtra("label") ?: ""), kind)
            }
            "accept" -> manager.accept()
        }
        if (manager.state.phase == CallPhase.IDLE) stopSelf()
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        handler.removeCallbacks(renewWakeLock)
        CallSession.serviceStopped()
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
        super.onDestroy()
    }

    companion object {
        fun start(context: Context, peer: Author, kind: String) {
            ContextCompat.startForegroundService(context, Intent(context, CallService::class.java)
                .setAction("start").putExtra("kind", kind).putExtra("peer", peer.accountId)
                .putExtra("handle", peer.handle).putExtra("label", peer.label))
        }

        fun accept(context: Context, kind: String) {
            ContextCompat.startForegroundService(context, Intent(context, CallService::class.java)
                .setAction("accept").putExtra("kind", kind))
        }
    }
}