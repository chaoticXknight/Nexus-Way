package com.nexusway.connect

import android.app.Notification
import android.app.NotificationManager
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.uiautomator.By
import androidx.test.uiautomator.Condition // Observe Android's actual ringtone owner instead of only the notification's sound URI.
import androidx.test.uiautomator.UiDevice
import androidx.test.uiautomator.Until
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch // Await an observable call-state transition without fixed sleeps.
import java.util.concurrent.TimeUnit // Bound the direct Answer regression so failures cannot hang the device test.

@RunWith(AndroidJUnit4::class)
class CallDeviceTest { // These synthetic calls never contact another account or connect remote media.
    private val instrumentation = InstrumentationRegistry.getInstrumentation() // Run WebRTC and Compose operations on Android's main thread.
    private val context = instrumentation.targetContext // Exercise the installed signed application, including its real manifest and resources.

    private fun invite(callId: String) = JSONObject().put("call_id", callId).put("from", "local-call-test") // Synthetic frames are passed only to the media manager, not the trusted relay receiver.
        .put("action", "invite").put("kind", "voice").put("payload", "e30=") // An empty payload creates a local ringing state without network access.
        .put("expires_at", System.currentTimeMillis() / 1_000 + 60) // Bound every synthetic notification to one minute even if a test fails.

    @Test fun speakerAndHangupDoNotPassNullToWebRtc() { // Reproduce both AudioDeviceInfo null crashes captured on the connected phone.
        instrumentation.runOnMainSync {
            val manager = SecureCallManager(context, sendSignal = {}) // No signals from this isolated manager leave the device.
            try {
                manager.handle(invite("audio-route-regression"), "Local audio test") // Establish only the local incoming state.
                manager.accept() // Prepare real WebRTC audio devices without connecting a remote peer.
                manager.toggleSpeaker() // Previously passed null to WebRTC and crashed immediately.
                manager.toggleSpeaker() // Verify the handset route also selects an input, not an output device.
                manager.hangup() // Previously passed null during cleanup and crashed the process.
                assertEquals(CallPhase.IDLE, manager.state.phase) // A successful hangup must return to idle without an exception.
            } finally { manager.dispose() } // Release native tracks and audio focus even after a failed assertion.
        }
    }

    @Test fun incomingNotificationAndCallScreenHaveCallerAndActions() { // Verify actual Android notification fields and rendered controls.
        val callId = "notification-regression"
        val frame = invite(callId)
        val device = UiDevice.getInstance(instrumentation) // Exercise the actual screen-off full-screen-notification path.
        device.sleep() // Lock the display before delivery instead of manually starting an activity over an unlocked phone.
        instrumentation.runOnMainSync {
            assumeTrue(Store(context).loadPendingCall() == null) // Never overwrite a real pending invitation.
            assumeTrue(CallSession.current?.state?.phase.let { it == null || it == CallPhase.IDLE }) // Never interrupt a real active call.
            CallSession.manager().handle(frame, "Local call test") // Create a clearly labeled synthetic incoming call.
        }
        try {
            instrumentation.runOnMainSync {
                assertTrue(ConnectNotifications.postIncomingCall(context, frame)) // Notification permission must be granted on the test phone.
                val manager = context.getSystemService(NotificationManager::class.java)
                val notification = manager.activeNotifications.single { it.id == callId.hashCode() }.notification // Inspect the system-posted notification, not just its builder.
                assertNotNull(notification.fullScreenIntent) // A locked phone needs an actual full-screen PendingIntent.
                assertEquals(Notification.CATEGORY_CALL, notification.category)
                assertEquals("Local call test", notification.extras.getCharSequence(Notification.EXTRA_TITLE).toString())
                assertTrue(notification.actions.any { it.title.toString().contains("Answer", true) }) // Verify Android supplied a direct answer control.
                assertTrue(notification.actions.any { it.title.toString().contains("Decline", true) }) // Verify Android supplied a direct decline control.
                assertNotNull(manager.getNotificationChannel(notification.channelId).sound) // The previous channel was permanently silent.
                assertTrue(manager.getNotificationChannel(notification.channelId).shouldVibrate()) // Respect phones configured to vibrate.
            }
            assertTrue(device.wait(Until.hasObject(By.text("Local call test")), 5_000)) // Verify the caller fits into the real rendered UI tree.
            assertFalse(device.wait(Until.gone(By.text("Local call test")), 5_000)) // The native popup must remain visible instead of disappearing after the initial wake.
            assertTrue(device.hasObject(By.text("Accept"))) // Check the visible answer control.
            assertTrue(device.hasObject(By.text("Decline"))) // Check the visible decline control.
            device.findObject(By.desc("Decline")).click() // End the call through the same control the user taps.
            assertTrue(device.wait(Until.gone(By.text("Local call test")), 5_000)) // The ended call must close its lock-screen activity.
            instrumentation.runOnMainSync { assertEquals(CallPhase.IDLE, CallSession.current?.state?.phase) } // Verify rejection actually changed call state.
        } finally {
            instrumentation.runOnMainSync {
                CallSession.current?.hangup() // Dismiss the synthetic notification and ringtone after the check.
            }
        }
    }

    @Test fun notificationAnswerStartsForegroundCallWithoutUnlocking() { // Cover the direct native Answer action, not just the in-app button.
        val callId = "notification-answer-regression"
        val frame = invite(callId)
        val answered = CountDownLatch(1) // Signal success only after the foreground service advances the call state.
        val device = UiDevice.getInstance(instrumentation)
        device.sleep() // Start from a locked display, as a real incoming phone call would.
        instrumentation.runOnMainSync {
            assumeTrue(Store(context).loadPendingCall() == null) // Leave real invitations untouched.
            assumeTrue(CallSession.current?.state?.phase.let { it == null || it == CallPhase.IDLE }) // Never interrupt an actual conversation.
            CallSession.manager().handle(frame, "Local answer test") // The process client is unconfigured, so synthetic signaling cannot leave the phone.
            assertTrue(ConnectNotifications.postIncomingCall(context, frame))
        }
        try {
            assertTrue(device.wait(Until.hasObject(By.text("Local answer test")), 5_000)) // Wait for the real full-screen activity, not a manually launched test host.
            instrumentation.runOnMainSync {
                val notification = context.getSystemService(NotificationManager::class.java).activeNotifications.single { it.id == callId.hashCode() }.notification
                notification.actions.first { it.title.toString().contains("Answer", true) }.actionIntent.send() // Invoke exactly the PendingIntent Android's native Answer button uses.
                val handler = android.os.Handler(android.os.Looper.getMainLooper()) // Observe the asynchronous permission and foreground-service transition on its owning thread.
                val deadline = android.os.SystemClock.uptimeMillis() + 5_000 // Stop checking when the assertion window closes.
                handler.post(object : Runnable {
                    override fun run() {
                        val state = CallSession.current?.state
                        if (state?.callId == callId && state.phase == CallPhase.CONNECTING) answered.countDown() // The answer must prepare media through CallService.
                        else if (android.os.SystemClock.uptimeMillis() < deadline) handler.postDelayed(this, 50) // Do not block the main thread while Android handles the intent.
                    }
                })
            }
            assertTrue("Notification Answer did not start the foreground call", answered.await(6, TimeUnit.SECONDS))
        } finally {
            instrumentation.runOnMainSync { CallSession.current?.hangup() } // Stop the synthetic call service and release microphone resources.
        }
    }

    @Test fun callerNameRefreshKeepsRingtonePlaying() { // Reproduce the metadata update that silenced real incoming calls.
        val device = UiDevice.getInstance(instrumentation)
        val frame = invite("ringtone-refresh-regression")
        val ringing = object : Condition<UiDevice, Boolean> { // Read the system notification service's active ringtone owner.
            override fun apply(device: UiDevice): Boolean = device.executeShellCommand("dumpsys notification") // This is privileged test observation, not an application permission.
                .lineSequence().any { it.contains("mSoundNotificationKey=") && it.contains("com.nexusway.connect") } // A configured sound URI alone does not prove playback.
        }
        device.sleep() // Include full-screen presentation in the real incoming-ring path.
        instrumentation.runOnMainSync {
            assumeTrue(Store(context).loadPendingCall() == null) // Do not overwrite a real incoming invitation.
            assumeTrue(CallSession.current?.state?.phase.let { it == null || it == CallPhase.IDLE }) // Do not disturb a real conversation.
            CallSession.manager().handle(frame, "Ringtone test") // Local synthetic calls never configure a network client.
        }
        try {
            instrumentation.runOnMainSync { assertTrue(ConnectNotifications.postIncomingCall(context, frame)) }
            assertTrue("Android did not start the incoming ringtone", device.wait(ringing, 5_000)) // Establish that initial notification playback works.
            instrumentation.runOnMainSync {
                CallSession.manager().updatePeerLabel("Resolved ringtone test") // Simulate the authenticated profile lookup completing.
                assertTrue(ConnectNotifications.postIncomingCall(context, frame)) // Exercise the actual caller-name notification refresh.
            }
            assertTrue(device.wait(Until.hasObject(By.text("Resolved ringtone test")), 5_000)) // Wait until the caller-name update reaches the visible call screen.
            assertFalse(device.wait(Until.gone(By.text("Resolved ringtone test")), 2_000)) // Let Android process the notification update while the call stays incoming.
            assertTrue("Caller-name refresh stopped Android's repeating ringtone", device.wait(ringing, 2_000)) // Detect the silent-update regression on the actual device.
        } finally {
            instrumentation.runOnMainSync { CallSession.current?.hangup() } // Cancel the test ringtone even after a failing assertion.
        }
    }
}