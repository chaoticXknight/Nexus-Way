package com.nexusway.connect

/**
 * Owns the Compose call screen and WebRTC video renderer attachment lifecycle.
 * It does not negotiate or route calls; SecureCallManager owns call state and
 * ConnectViewModel sends signed signaling.
 */

import android.Manifest
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.CallEnd
import androidx.compose.material.icons.outlined.Cameraswitch
import androidx.compose.material.icons.outlined.Mic
import androidx.compose.material.icons.outlined.MicOff
import androidx.compose.material.icons.outlined.VolumeOff
import androidx.compose.material.icons.outlined.VolumeUp
import androidx.compose.material.icons.outlined.Videocam
import androidx.compose.material.icons.outlined.VideocamOff
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import org.webrtc.EglBase
import org.webrtc.SurfaceViewRenderer
import org.webrtc.VideoTrack

@Composable
fun CallPane(vm: ConnectViewModel) {
    val manager = CallSession.current ?: return // Reuse the service-owned call without exposing view-model internals.
    CallPane(manager, vm::acceptCall) { vm.errorDialog = it } // Keep the main screen's existing permission-error handling.
}

@Composable
fun CallPane(manager: SecureCallManager, onAccept: () -> Unit, onError: (String) -> Unit) { // Host the same controls in either activity.
    val call = manager.state // Observe the process-owned call state.
    var accepting by remember { mutableStateOf(false) }
    var controlsVisible by remember(call.callId) { mutableStateOf(true) }
    val connectionWarning = call.status != "Secure call" || call.quality.startsWith("Unstable")
    LaunchedEffect(call.phase, connectionWarning) {
        if (call.phase == CallPhase.INCOMING || connectionWarning) controlsVisible = true
    }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        if (accepting && grants.values.all { it }) onAccept() // Capture starts only after permission is granted.
        else if (accepting) onError("Microphone and camera permission are required for this call") // Let the host present the permission error.
        accepting = false
    }
    BackHandler { manager.hangup() } // Preserve explicit call-ending behavior on Back.

    Box(Modifier.fillMaxSize().background(Bg)) {
        HiveHoneycombBackground()
        if (call.kind == "video" && call.phase != CallPhase.INCOMING) {
            call.remoteVideo?.let {
                VideoSurface(it, manager.eglContext, mirror = false, overlay = false, Modifier.fillMaxSize()) // Use the shared remote video context.
            }
            call.localVideo?.let {
                Surface(
                    shape = HiveCutShape,
                    color = Panel,
                    border = BorderStroke(1.dp, Accent2),
                    modifier = Modifier
                        .align(Alignment.TopEnd)
                        .padding(12.dp)
                        .width(108.dp)
                        .aspectRatio(3f / 4f)
                        .hivePanelDepth(active = true),
                ) {
                        VideoSurface(it, manager.eglContext, mirror = true, overlay = true, Modifier.fillMaxSize()) // Use the shared local video context.
                }
            }
        }

        if (call.phase == CallPhase.CONNECTING || call.phase == CallPhase.ACTIVE) {
            Box(
                Modifier
                    .fillMaxSize()
                    .pointerInput(call.callId) {
                        detectTapGestures { controlsVisible = !controlsVisible }
                    },
            )
        }

        AnimatedVisibility(
            visible = call.phase == CallPhase.INCOMING || controlsVisible || connectionWarning,
            enter = fadeIn(),
            exit = fadeOut(),
        ) {
            Column(
                Modifier.fillMaxSize().padding(horizontal = 16.dp, vertical = 20.dp),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.SpaceBetween,
            ) {
                Surface(
                    color = Panel.copy(alpha = 0.94f),
                    shape = HiveCutShape,
                    border = BorderStroke(1.dp, BorderSoft),
                    modifier = Modifier.fillMaxWidth().hivePanelDepth(),
                ) {
                    Column(
                        Modifier.padding(horizontal = 18.dp, vertical = 14.dp),
                        horizontalAlignment = Alignment.CenterHorizontally,
                    ) {
                        Text(call.peerLabel, color = TextMain, fontSize = 22.sp, fontWeight = FontWeight.Bold)
                        Spacer(Modifier.height(6.dp))
                        Text(call.status, color = Accent2, fontSize = 14.sp, fontWeight = FontWeight.SemiBold)
                        if (call.quality.isNotEmpty()) {
                            Spacer(Modifier.height(3.dp))
                            Text(call.quality, color = TextDim, fontSize = 11.sp)
                        }
                        Spacer(Modifier.height(3.dp))
                        Text("DTLS-SRTP encrypted", color = TextDim, fontSize = 12.sp)
                    }
                }

                if (call.kind == "voice" || call.phase == CallPhase.INCOMING) {
                    Surface(
                        shape = HiveCutShape,
                        color = AccentSoft,
                        border = BorderStroke(1.dp, Border),
                        modifier = Modifier.size(132.dp).hivePanelDepth(active = true),
                    ) {
                        Box(contentAlignment = Alignment.Center) {
                            Icon(
                                if (call.kind == "video") Icons.Outlined.Videocam else Icons.Outlined.Call,
                                contentDescription = null,
                                tint = Accent2,
                                modifier = Modifier.size(58.dp),
                            )
                        }
                    }
                } else {
                    Spacer(Modifier.height(132.dp))
                }

                Surface(
                    color = Panel.copy(alpha = 0.94f),
                    shape = HiveCutShape,
                    border = BorderStroke(1.dp, BorderSoft),
                    modifier = Modifier.fillMaxWidth().hivePanelDepth(),
                ) {
                    Box(Modifier.fillMaxWidth().padding(horizontal = 14.dp, vertical = 12.dp)) {
                        if (call.phase == CallPhase.INCOMING) {
                            Row(
                                horizontalArrangement = Arrangement.spacedBy(28.dp),
                                modifier = Modifier.align(Alignment.Center),
                            ) {
                                CallAction(Icons.Outlined.CallEnd, "Decline", Danger, manager::reject) // Reject the verified incoming call.
                                CallAction(Icons.Outlined.Call, "Accept", Accent2) {
                                    accepting = true
                                    permissionLauncher.launch(callPermissions(call.kind))
                                }
                            }
                        } else {
                            Column(
                                Modifier.fillMaxWidth(),
                                horizontalAlignment = Alignment.CenterHorizontally,
                                verticalArrangement = Arrangement.spacedBy(12.dp),
                            ) {
                                Row(
                                    horizontalArrangement = Arrangement.spacedBy(14.dp),
                                    verticalAlignment = Alignment.CenterVertically,
                                ) {
                                    CallAction(
                                        if (call.muted) Icons.Outlined.MicOff else Icons.Outlined.Mic,
                                        if (call.muted) "Unmute" else "Mute",
                                        PanelHi,
                                        manager::toggleMute, // Toggle the shared microphone.
                                    )
                                    CallAction(
                                        if (call.speakerEnabled) Icons.Outlined.VolumeUp else Icons.Outlined.VolumeOff,
                                        "Speaker",
                                        if (call.speakerEnabled) AccentSoft else PanelHi,
                                        manager::toggleSpeaker, // Toggle the shared speaker route.
                                    )
                                    if (call.kind == "video") {
                                        CallAction(Icons.Outlined.Cameraswitch, "Switch", PanelHi, manager::switchCamera) // Switch the active camera.
                                    } else {
                                        CallAction(Icons.Outlined.CallEnd, "Hang up", Danger, manager::hangup) // End the voice call.
                                    }
                                }
                                if (call.kind == "video") {
                                    Row(
                                        horizontalArrangement = Arrangement.spacedBy(14.dp),
                                        verticalAlignment = Alignment.CenterVertically,
                                    ) {
                                        CallAction(
                                            if (call.cameraEnabled) Icons.Outlined.Videocam else Icons.Outlined.VideocamOff,
                                            "Camera",
                                            PanelHi,
                                            manager::toggleCamera, // Toggle the shared camera.
                                        )
                                        CallAction(Icons.Outlined.CallEnd, "Hang up", Danger, manager::hangup) // End the video call.
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun CallAction(
    icon: androidx.compose.ui.graphics.vector.ImageVector,
    label: String,
    color: Color,
    onClick: () -> Unit,
) {
    Column(horizontalAlignment = Alignment.CenterHorizontally) {
        Button(
            onClick = onClick,
            shape = HiveCutShape,
            colors = ButtonDefaults.buttonColors(containerColor = color),
            border = BorderStroke(1.dp, Border),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(0.dp),
            modifier = Modifier.size(56.dp),
        ) { Icon(icon, label, tint = TextMain) }
        Spacer(Modifier.height(6.dp))
        Text(label, color = TextDim, fontSize = 11.sp)
    }
}

@Composable
private fun VideoSurface(
    track: VideoTrack,
    eglContext: EglBase.Context,
    mirror: Boolean,
    overlay: Boolean,
    modifier: Modifier,
) {
    val context = LocalContext.current
    val renderer = remember {
        SurfaceViewRenderer(context).apply {
            init(eglContext, null)
            setEnableHardwareScaler(true)
            setZOrderMediaOverlay(overlay)
        }
    }
    renderer.setMirror(mirror)
    DisposableEffect(track, renderer) {
        track.addSink(renderer)
        onDispose {
            track.removeSink(renderer)
            renderer.release()
        }
    }
    AndroidView(factory = { renderer }, modifier = modifier)
}