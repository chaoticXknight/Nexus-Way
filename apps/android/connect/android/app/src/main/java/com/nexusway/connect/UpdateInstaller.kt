// Owns silent in-app APK installation through PackageInstaller sessions.
// ConnectViewModel owns download + SHA-256 verification and calls commit()
// with an already-verified file; this module owns the install session and
// its result handling. The flow:
//   - Connect self-updates (or updates Nexus Notify) via a session commit.
//   - The FIRST install/update through this path shows the one system
//     confirmation sheet (STATUS_PENDING_USER_ACTION) because the current
//     installer of record is whoever sideloaded the app.
//   - After that, Connect IS the installer of record, and on Android 12+
//     (UPDATE_PACKAGES_WITHOUT_USER_ACTION + setRequireUserAction(false))
//     subsequent updates apply silently in the background.
package com.nexusway.connect

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.util.Log
import java.io.File

object UpdateInstaller {
    private const val TAG = "NexusUpdate"
    private const val ACTION_RESULT = "com.nexusway.connect.INSTALL_RESULT"

    /**
     * Commit a verified APK through a PackageInstaller session. Returns
     * immediately; the session result arrives at [UpdateResultReceiver]
     * (silent success, a user-confirmation launch, or a logged failure).
     * A successful SELF-update kills and restarts this process by design.
     */
    fun commit(context: Context, apk: File, targetPackage: String) {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(
            PackageInstaller.SessionParams.MODE_FULL_INSTALL,
        ).apply {
            setAppPackageName(targetPackage)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                // Silent when we are the installer of record; otherwise the
                // platform downgrades this to a normal confirmation prompt.
                setRequireUserAction(
                    PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED,
                )
            }
        }
        val sessionId = installer.createSession(params)
        installer.openSession(sessionId).use { session ->
            session.openWrite("apk", 0, apk.length()).use { out ->
                apk.inputStream().use { it.copyTo(out) }
                session.fsync(out)
            }
            val intent = Intent(ACTION_RESULT).apply {
                setPackage(context.packageName)
                putExtra("target", targetPackage)
            }
            val pending = PendingIntent.getBroadcast(
                context,
                sessionId,
                intent,
                // Mutable: the installer fills in the status extras.
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
            )
            session.commit(pending.intentSender)
        }
        Log.i(TAG, "install session $sessionId committed for $targetPackage")
    }
}

/** Receives PackageInstaller session results for in-app updates. */
class UpdateResultReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val status = intent.getIntExtra(
            PackageInstaller.EXTRA_STATUS,
            PackageInstaller.STATUS_FAILURE,
        )
        val target = intent.getStringExtra("target") ?: "?"
        when (status) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                // Not (yet) the installer of record — hand the user the
                // system confirmation sheet. After this one, we are.
                val confirm: Intent? = if (Build.VERSION.SDK_INT >= 33) {
                    intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                } else {
                    @Suppress("DEPRECATION")
                    intent.getParcelableExtra(Intent.EXTRA_INTENT)
                }
                confirm?.let {
                    it.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                    runCatching { context.startActivity(it) }
                        .onFailure { e ->
                            Log.w("NexusUpdate", "confirmation launch failed", e)
                        }
                }
            }
            PackageInstaller.STATUS_SUCCESS -> {
                Log.i("NexusUpdate", "install succeeded for $target")
                if (target == "com.nexusway.notify") NexusNotifier.ensureRunning(context)
            }
            else ->
                Log.w(
                    "NexusUpdate",
                    "install failed for $target status=$status " +
                        intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE).orEmpty(),
                )
        }
    }
}
