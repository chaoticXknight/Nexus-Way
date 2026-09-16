package com.nexusway.connect

import android.content.Context
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.*
import okhttp3.WebSocket
import org.json.JSONObject

object CallSession {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private val inbox = CallSignalInbox()
    private val signals = inbox.frames
    private val deviceKeys = mutableMapOf<String, ByteArray>()
    private val seenSignals = LinkedHashSet<String>()
    private var context: Context? = null
    private var client: HiveClient? = null
    private var enrollment: Enrollment? = null
    private var socket: WebSocket? = null
    private var reconnect: Job? = null
    private var generation = 0L
    private var visible = false
    private var serviceRunning = false
    private var previousPhase = CallPhase.IDLE
    var current by mutableStateOf<SecureCallManager?>(null)
        private set

    init {
        scope.launch {
            for (frame in signals) {
                try {
                    handle(frame)
                } catch (error: CancellationException) {
                    throw error
                } catch (error: Exception) {
                    Log.w("NexusCall", "call signal processing failed", error)
                }
            }
        }
    }

    fun initialize(application: Context) { context = application.applicationContext }

    suspend fun restoreCall(callId: String): Boolean = withContext(Dispatchers.Main.immediate) { // Lock-screen actions must refer to a live, verified call.
        if (callId.isBlank()) return@withContext false // Missing notification IDs cannot target the current call.
        if (current?.state?.callId != callId) { // Cold-start restoration uses the same signature checks as live delivery.
            val app = context ?: return@withContext false
            val frame = Store(app).loadPendingCall() ?: return@withContext false
            if (frame.optString("call_id") != callId) return@withContext false // Never substitute a newer pending call.
            handle(frame) // Verify account, device, signature, and invite expiration before presenting controls.
        }
        current?.state?.let { it.callId == callId && it.phase != CallPhase.IDLE } == true // Completed calls stay completed.
    }

    fun configure(application: Context, api: HiveClient, identity: Enrollment) {
        initialize(application)
        if (enrollment?.accountId != identity.accountId) {
            deviceKeys.clear()
            seenSignals.clear()
        }
        if (client !== api) disconnect()
        client = api
        enrollment = identity
        connect()
    }

    fun setVisible(active: Boolean) {
        visible = active
        Log.i("NexusCall", "app visible=$active callService=$serviceRunning")
        if (visible || serviceRunning) connect() else disconnect()
    }

    fun receive(frame: JSONObject) {
        if (!inbox.submit(frame)) {
            Log.e("NexusCall", "call signaling queue full; ending call rather than losing control frames")
            scope.launch { current?.hangup() }
        }
    }

    private suspend fun handle(frame: JSONObject) {
        val app = context ?: return
        val identity = enrollment ?: Store(app).load()?.also { enrollment = it } ?: return
        val api = client ?: SessionManager.client(app, identity).also { client = it }
        val from = frame.optString("from")
        val deviceId = frame.optString("sender_device")
        val keyId = "$from:$deviceId"
        val publicKey = deviceKeys[keyId] ?: run {
            val directory = api.wireDirectory(from)
            val devices = directory.optJSONArray("devices") ?: return
            val device = (0 until devices.length()).map { devices.getJSONObject(it) }
                .firstOrNull { it.optString("device_id") == deviceId } ?: return
            if (!WireCrypto.validateDevice(from, directory.optString("identity_pub"), device)) return
            unb64(device.getString("device_pub")).also { deviceKeys[keyId] = it }
        }
        val signed = WireCrypto.signedCall(frame.optString("call_id"), frame.optString("action"),
            frame.optString("kind"), frame.optString("payload"))
        if (!Key.verify(publicKey, signed.toByteArray(), unb64(frame.getString("signature")))) return
        val action = frame.optString("action")
        if (action == "invite" && frame.optLong("expires_at", Long.MAX_VALUE) <= System.currentTimeMillis() / 1_000) {
            if (ConnectNotifications.postMissedCall(app, frame.optString("call_id"), frame.optString("kind"))) {
                NexusNotifier.acknowledge(app, frame.optString("relay_id"))
            }
            return
        }
        val fingerprint = sha256hex("$keyId:$signed:${frame.optString("signature")}".toByteArray())
        if (action !in setOf("heartbeat", "heartbeat_ack") && fingerprint in seenSignals) {
            if (action != "invite" || current?.state?.phase != CallPhase.INCOMING ||
                ConnectNotifications.postIncomingCall(app, frame)
            ) NexusNotifier.acknowledge(app, frame.optString("relay_id"))
            return
        }
        val manager = manager()
        if (action == "invite") {
            manager.setRelayServers(runCatching { api.callIceServers() }.getOrDefault(emptyList()))
            if (manager.state.phase == CallPhase.IDLE || manager.state.callId == frame.optString("call_id")) {
                Store(app).savePendingCall(frame)
            }
        }
        val knownHandle = if (action == "invite") Store(app).loadDirectMessages().firstOrNull { it.peerAccount == from }?.peerHandle.orEmpty() else "" // Verified local history remains available when profile lookup is offline.
        val profile = if (action == "invite" && manager.state.phase == CallPhase.IDLE) { // Resolve identity before the first alert; updating a ringing notification can silence Android's ringtone.
            try { withTimeoutOrNull(3_000) { api.profileGet(from) } } catch (error: Exception) { // Keep offline profile lookup bounded to three seconds.
                if (error is CancellationException) throw error // Preserve cancellation when the owning coroutine stops.
                null // Failed lookups use the verified local handle or account fallback.
            }
        } else null // Other signaling and busy-call rejection do not wait for profile requests.
        manager.handle(frame, incomingCallerLabel(from, profile, knownHandle)) // Give the heads-up notification and full-screen view the same verified initial caller name.
        if (action in setOf("hangup", "reject", "accept")) {
            Store(app).clearPendingCall(frame.optString("call_id"))
            CallRinger.stop(frame.optString("call_id"))
            ConnectNotifications.cancelIncomingCall(app, frame.optString("call_id"))
        }
        val notified = action != "invite" || manager.state.callId != frame.optString("call_id") ||
            ConnectNotifications.postIncomingCall(app, frame)
        if (notified) NexusNotifier.acknowledge(app, frame.optString("relay_id"))
        if (action !in setOf("heartbeat", "heartbeat_ack")) {
            seenSignals.add(fingerprint)
            while (seenSignals.size > 512) seenSignals.remove(seenSignals.first())
        }
    }

    fun manager(): SecureCallManager = current ?: SecureCallManager(
        requireNotNull(context),
        sendSignal = { signal ->
            val api = client
            val identity = enrollment
            scope.launch {
                try {
                    api?.wireCallSignal(signal.target, signal.callId, signal.action, signal.kind,
                        identity?.device ?: return@launch, signal.payload)
                } catch (error: Exception) {
                    if (error is CancellationException) throw error
                    Log.w("NexusCall", "call signaling send failed action=${signal.action}", error)
                }
            }
        },
        onCallFailure = { Log.w("NexusCall", it) },
        onStateChanged = { state ->
            val ended = previousPhase != CallPhase.IDLE && state.phase == CallPhase.IDLE
            previousPhase = state.phase
            if (ended) context?.stopService(android.content.Intent(context, CallService::class.java))
        },
    ).also { current = it }

    fun serviceStarted() { serviceRunning = true; connect() }

    fun serviceStopped() {
        serviceRunning = false
        if (current?.state?.phase in setOf(CallPhase.ACTIVE, CallPhase.CONNECTING)) current?.hangup()
        if (!visible) disconnect()
    }

    fun clear() {
        current?.hangup()
        current?.dispose()
        current = null
        disconnect()
        client = null
        enrollment = null
        deviceKeys.clear()
        seenSignals.clear()
        while (signals.tryReceive().isSuccess) Unit
    }

    private fun disconnect() {
        generation++
        reconnect?.cancel()
        reconnect = null
        socket?.cancel()
        socket = null
    }

    private fun connect() {
        val api = client ?: return
        if ((!visible && !serviceRunning) || socket != null) return
        val owner = ++generation
        socket = api.stream(
            onFrame = { frame ->
                if (frame.optString("type") == "call_signal") receive(frame)
                else HiveStreamEvents.tryEmit(frame)
            },
            onClosed = {
                scope.launch {
                    if (owner != generation) return@launch
                    socket = null
                    if (visible || serviceRunning) {
                        reconnect = launch { delay(2_000); connect() }
                    }
                }
            },
        )
    }
}