package com.nexusway.connect

import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.view.WindowManager
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.withResumed // Notification Answer must wait until Android considers this activity fully visible.
import kotlinx.coroutines.launch

class IncomingCallActivity : ComponentActivity() { // This activity exposes only call controls while the phone remains locked.
    private var requestedCallId by mutableStateOf("") // Bind every notification action to its original call.
    private var ready by mutableStateOf(false) // Wait for saved signals to be verified before showing controls.
    private var pendingAction: String? = null // Consume Answer or Decline once, including across resume.
    private val permissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { grants -> // Let Android own microphone and camera consent.
        if (grants.values.all { it }) accept() else showError("Microphone and camera permission are required for this call") // Do not capture media after a denial.
    }

    override fun onCreate(savedInstanceState: Bundle?) { // A separate task prevents the lock screen from exposing messages or account settings.
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE) // Preserve Connect's screen-capture protection.
        openCall(intent) // Restore only the verified call associated with this notification.
        setContent { // Reuse the existing editable call controls and visual language.
            MaterialTheme {
                val manager = CallSession.current // Observe the service-owned manager without creating another app view model.
                val state = manager?.state // Recompose when the call ends or changes phase.
                LaunchedEffect(ready, state?.phase, state?.callId, requestedCallId) { // Close stale screens instead of showing controls for another caller.
                    if (ready && (state == null || state.phase == CallPhase.IDLE || state.callId != requestedCallId)) finish()
                }
                if (ready && manager != null && state?.callId == requestedCallId && state.phase != CallPhase.IDLE) { // Never expose unrelated application content above the keyguard.
                    Box(Modifier.fillMaxSize().windowInsetsPadding(WindowInsets.safeDrawing)) { // Keep controls clear of system bars on small phones.
                        CallPane(manager, ::accept, ::showError) // Answer starts the foreground service; the activity never owns media.
                    }
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) { // Handle notification actions when the call screen already exists.
        super.onNewIntent(intent)
        setIntent(intent)
        openCall(intent) // Revalidate the call ID for each new action.
    }

    private fun openCall(intent: Intent) { // Notification extras identify a call but never authorize a caller.
        requestedCallId = intent.getStringExtra(CALL_ID_EXTRA).orEmpty() // An absent ID cannot operate on the current call.
        pendingAction = intent.action // Save the explicit user action until the activity is resumed.
        ready = false // Avoid dismissing the screen during verification.
        lifecycleScope.launch {
            val callId = requestedCallId // Ignore completion of an older restoration after another intent arrives.
            val restored = runCatching { CallSession.restoreCall(callId) }.getOrDefault(false) // Reverify persisted frames through the normal signaling path.
            if (requestedCallId != callId) return@launch
            ready = true // Rendering and action handling can now use the verified state.
            if (!restored) finish() else performAction() // Expired or completed calls cannot be answered.
        }
    }

    override fun onResume() { // Android requires a visible activity before starting microphone capture.
        super.onResume()
        lifecycleScope.launch { lifecycle.withResumed { performAction() } } // ComponentActivity may still be STARTED inside onResume itself.
    }

    private fun performAction() { // Consume notification actions only while this call screen is visible.
        if (!ready || !lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) return
        val state = CallSession.current?.state ?: return // The call may have ended during activity startup.
        if (state.callId != requestedCallId || state.phase != CallPhase.INCOMING) return // Stale actions never affect active or replacement calls.
        val action = pendingAction // Capture before clearing so configuration changes cannot repeat an answer.
        pendingAction = null
        intent.action = OPEN_CALL_ACTION // Do not replay Answer after activity recreation.
        when (action) {
            ANSWER_CALL_ACTION -> permissions.launch(callPermissions(state.kind)) // Request capture access before accepting from the notification.
            DECLINE_CALL_ACTION -> CallSession.current?.reject() // Send rejection through the existing signed signaling channel.
        }
    }

    private fun accept() { // Both the notification and on-screen Answer button use this foreground entry point.
        val state = CallSession.current?.state ?: return
        if (state.callId != requestedCallId || state.phase != CallPhase.INCOMING) return // Recheck after a potentially long permission prompt.
        if (callPermissions(state.kind).any { ContextCompat.checkSelfPermission(this, it) != PackageManager.PERMISSION_GRANTED }) return // Never start capture without consent.
        runCatching { CallService.accept(this, state.kind) }.onFailure { showError("Could not start the call service") } // Retain the call screen when service startup fails.
    }

    private fun showError(message: String) { // Permission and service failures remain visible without opening the main app.
        Toast.makeText(this, message, Toast.LENGTH_LONG).show()
    }
}