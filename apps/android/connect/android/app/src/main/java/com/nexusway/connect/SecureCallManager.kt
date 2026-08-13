package com.nexusway.connect

/**
 * Owns WebRTC peer, camera, microphone, audio-route, ringtone, and call-state
 * lifecycles. It does not authorize peers or transport signaling; HIVE and
 * ConnectViewModel own those boundaries.
 */

import android.Manifest
import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFormat
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.AudioTrack as AndroidAudioTrack
import android.media.MediaRecorder
import android.media.Ringtone
import android.media.RingtoneManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import java.security.SecureRandom
import kotlin.math.PI
import kotlin.math.sin
import org.json.JSONObject
import org.webrtc.AudioSource
import org.webrtc.AudioTrack
import org.webrtc.Camera2Enumerator
import org.webrtc.CameraVideoCapturer
import org.webrtc.DataChannel
import org.webrtc.EglBase
import org.webrtc.IceCandidate
import org.webrtc.MediaConstraints
import org.webrtc.MediaStream
import org.webrtc.MediaStreamTrack
import org.webrtc.PeerConnection
import org.webrtc.PeerConnectionFactory
import org.webrtc.RtpReceiver
import org.webrtc.RtpParameters
import org.webrtc.RtpSender
import org.webrtc.RtpTransceiver
import org.webrtc.SdpObserver
import org.webrtc.SessionDescription
import org.webrtc.SurfaceTextureHelper
import org.webrtc.VideoSource
import org.webrtc.VideoTrack
import org.webrtc.audio.JavaAudioDeviceModule

fun callPermissions(kind: String): Array<String> = buildList {
    add(Manifest.permission.RECORD_AUDIO)
    if (kind == "video") add(Manifest.permission.CAMERA)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        add(Manifest.permission.BLUETOOTH_CONNECT)
    }
}.toTypedArray()

enum class CallPhase { IDLE, INCOMING, CONNECTING, ACTIVE }

data class CallUiState(
    val phase: CallPhase = CallPhase.IDLE,
    val callId: String = "",
    val peerAccount: String = "",
    val peerLabel: String = "",
    val kind: String = "voice",
    val outgoing: Boolean = false,
    val muted: Boolean = false,
    val speakerEnabled: Boolean = false,
    val cameraEnabled: Boolean = true,
    val localVideo: VideoTrack? = null,
    val remoteVideo: VideoTrack? = null,
    val status: String = "",
    val quality: String = "",
)

data class OutgoingCallSignal(
    val target: String,
    val callId: String,
    val action: String,
    val kind: String,
    val payload: JSONObject = JSONObject(),
)

object CallRinger {
    private val handler = Handler(Looper.getMainLooper())
    private val timeout = Runnable { stop() }
    private var callId: String? = null
    private var ringtone: Ringtone? = null

    @Synchronized
    fun start(context: Context, incomingCallId: String) {
        if (callId == incomingCallId && ringtone?.isPlaying == true) return
        stop()
        val uri = RingtoneManager.getActualDefaultRingtoneUri(
            context,
            RingtoneManager.TYPE_RINGTONE,
        ) ?: RingtoneManager.getDefaultUri(RingtoneManager.TYPE_RINGTONE)
        ringtone = RingtoneManager.getRingtone(context.applicationContext, uri)?.apply {
            audioAttributes = AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
                .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                .build()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) isLooping = true
            play()
        }
        callId = incomingCallId
        handler.postDelayed(timeout, 60_000)
    }

    @Synchronized
    fun stop(stoppingCallId: String? = null) {
        if (stoppingCallId != null && callId != stoppingCallId) return
        handler.removeCallbacks(timeout)
        ringtone?.stop()
        ringtone = null
        callId = null
    }
}

/** Plays the network-style ringback heard by the caller while an invite is pending. */
object CallRingback {
    private var callId: String? = null
    private var tracks: List<AndroidAudioTrack> = emptyList()
    private var audioManager: AudioManager? = null
    private var audioFocusRequest: AudioFocusRequest? = null

    @Synchronized
    fun start(context: Context, outgoingCallId: String) {
        if (callId == outgoingCallId && tracks.any {
                it.playState == AndroidAudioTrack.PLAYSTATE_PLAYING
            }
        ) return
        stop()
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_NOTIFICATION_RINGTONE)
            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
            .build()
        audioManager = context.applicationContext.getSystemService(AudioManager::class.java)
        audioFocusRequest = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT_MAY_DUCK)
            .setAudioAttributes(attributes)
            .build()
            .also { audioManager?.requestAudioFocus(it) }
        val outputs = audioManager?.getDevices(AudioManager.GET_DEVICES_OUTPUTS).orEmpty()
        val bluetooth = BLUETOOTH_OUTPUT_TYPES.firstNotNullOfOrNull { type ->
            outputs.firstOrNull { it.type == type }
        }
        val output = bluetooth
            ?: outputs.firstOrNull { it.type == AudioDeviceInfo.TYPE_BUILTIN_SPEAKER }
        val samples = ringbackSamples()
        tracks = listOfNotNull(output)
            .mapNotNull { device -> createTrack(attributes, samples, device) }
        if (tracks.isNotEmpty()) {
            callId = outgoingCallId
            Log.i("NexusCall", "ringback route=${output?.type}")
        } else {
            abandonAudioFocus()
        }
    }

    private fun createTrack(
        attributes: AudioAttributes,
        samples: ShortArray,
        device: AudioDeviceInfo,
    ): AndroidAudioTrack? = runCatching {
        AndroidAudioTrack.Builder()
            .setAudioAttributes(attributes)
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(RINGBACK_SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                    .build(),
            )
            .setBufferSizeInBytes(samples.size * Short.SIZE_BYTES)
            .setTransferMode(AndroidAudioTrack.MODE_STATIC)
            .build()
            .also { audioTrack ->
                check(audioTrack.write(samples, 0, samples.size) == samples.size)
                check(audioTrack.setPreferredDevice(device))
                check(audioTrack.setLoopPoints(0, samples.size, -1) == AndroidAudioTrack.SUCCESS)
                audioTrack.play()
            }
    }.onFailure {
        Log.w("NexusCall", "unable to start ringback route type=${device.type}", it)
    }.getOrNull()

    private fun ringbackSamples(): ShortArray {
        val samples = ShortArray(RINGBACK_SAMPLE_RATE * RINGBACK_CYCLE_SECONDS)
        repeat(RINGBACK_SAMPLE_RATE * RINGBACK_TONE_SECONDS) { sample ->
            val seconds = sample.toDouble() / RINGBACK_SAMPLE_RATE
            val signal = (sin(2.0 * PI * 440.0 * seconds) + sin(2.0 * PI * 480.0 * seconds)) / 2.0
            samples[sample] = (signal * Short.MAX_VALUE * 0.35).toInt().toShort()
        }
        return samples
    }

    private fun abandonAudioFocus() {
        audioFocusRequest?.let { request ->
            audioManager?.abandonAudioFocusRequest(request)
        }
        audioFocusRequest = null
        audioManager = null
    }

    private const val RINGBACK_SAMPLE_RATE = 8_000
    private const val RINGBACK_TONE_SECONDS = 2
    private const val RINGBACK_CYCLE_SECONDS = 6
    private val BLUETOOTH_OUTPUT_TYPES = listOf(
        AudioDeviceInfo.TYPE_BLE_HEADSET,
        AudioDeviceInfo.TYPE_BLUETOOTH_A2DP,
        AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
    )

    @Synchronized
    fun stop(stoppingCallId: String? = null) {
        if (stoppingCallId != null && callId != stoppingCallId) return
        tracks.forEach { audioTrack ->
            runCatching { audioTrack.stop() }
            audioTrack.release()
        }
        tracks = emptyList()
        callId = null
        abandonAudioFocus()
    }
}

class SecureCallManager(
    context: Context,
    private val sendSignal: (OutgoingCallSignal) -> Unit,
    private val onCallFailure: (String) -> Unit = {},
) {
    private val appContext = context.applicationContext
    private val main = Handler(Looper.getMainLooper())
    private val audioManager = appContext.getSystemService(AudioManager::class.java)
    private val eglBase = EglBase.create()
    private val audioDeviceModule: JavaAudioDeviceModule
    private val factory: PeerConnectionFactory
    private var peerConnection: PeerConnection? = null
    private var audioSource: AudioSource? = null
    private var audioTrack: AudioTrack? = null
    private var videoSource: VideoSource? = null
    private var videoCapturer: CameraVideoCapturer? = null
    private var surfaceTextureHelper: SurfaceTextureHelper? = null
    private var localVideoTrack: VideoTrack? = null
    private var audioConfigured = false
    private var previousAudioMode = AudioManager.MODE_NORMAL
    private var previousSpeakerphone = false
    private var audioFocusRequest: AudioFocusRequest? = null
    private var videoSender: RtpSender? = null
    private var lastOutboundBytes = 0L
    private var lastInboundBytes = 0L
    private var lastOutboundAudioBytes = 0L
    private var lastInboundAudioBytes = 0L
    private var lastPacketsLost = 0L
    private var lastStatsAtMs = 0L
    private var stalledOutboundAudioReports = 0
    private var lastMediaRecoveryAtMs = 0L
    private val pendingIce = mutableListOf<IceCandidate>()
    private var relayServers: List<CallIceServer> = emptyList()
    private val ringTimeout = Runnable {
        val timedOutCall = state
        if (state.phase == CallPhase.INCOMING || state.phase == CallPhase.CONNECTING) {
            close(sendHangup = true)
        }
        if (timedOutCall.phase == CallPhase.INCOMING) {
            ConnectNotifications.postMissedCall(
                appContext,
                timedOutCall.callId,
                timedOutCall.kind,
            )
        }
    }
    private val disconnectTimeout = Runnable {
        if (state.phase == CallPhase.ACTIVE || state.phase == CallPhase.CONNECTING) {
            val message = "Connection lost"
            close(sendHangup = false, finalStatus = message)
            onCallFailure(message)
        }
    }
    private val statsTick = object : Runnable {
        override fun run() {
            val connection = peerConnection ?: return
            if (state.phase != CallPhase.ACTIVE) return
            connection.getStats { report -> updateQuality(report.statsMap.values) }
            main.postDelayed(this, STATS_INTERVAL_MS)
        }
    }
    private val heartbeatTimeout = Runnable {
        if (state.phase == CallPhase.ACTIVE || state.phase == CallPhase.CONNECTING) {
            Log.w("NexusCall", "peer heartbeat timed out")
            state = state.copy(status = "Call signaling interrupted")
        }
    }
    private val heartbeatTick = object : Runnable {
        override fun run() {
            if (state.phase != CallPhase.ACTIVE) return
            emit("heartbeat")
            main.postDelayed(this, HEARTBEAT_INTERVAL_MS)
        }
    }

    var state by mutableStateOf(CallUiState())
        private set

    val eglContext: EglBase.Context get() = eglBase.eglBaseContext

    fun setRelayServers(servers: List<CallIceServer>) {
        if (peerConnection == null) relayServers = servers
    }

    init {
        PeerConnectionFactory.initialize(
            PeerConnectionFactory.InitializationOptions.builder(appContext)
                .setEnableInternalTracer(false)
                .createInitializationOptions(),
        )
        audioDeviceModule = JavaAudioDeviceModule.builder(appContext)
            .setAudioSource(MediaRecorder.AudioSource.VOICE_COMMUNICATION)
            .setUseHardwareAcousticEchoCanceler(
                JavaAudioDeviceModule.isBuiltInAcousticEchoCancelerSupported(),
            )
            .setUseHardwareNoiseSuppressor(
                JavaAudioDeviceModule.isBuiltInNoiseSuppressorSupported(),
            )
            .setUseLowLatency(true)
            .setUseStereoInput(false)
            .setUseStereoOutput(false)
            .createAudioDeviceModule()
        factory = PeerConnectionFactory.builder()
            .setAudioDeviceModule(audioDeviceModule)
            .setVideoEncoderFactory(org.webrtc.DefaultVideoEncoderFactory(eglContext, true, true))
            .setVideoDecoderFactory(org.webrtc.DefaultVideoDecoderFactory(eglContext))
            .createPeerConnectionFactory()
    }

    fun start(peer: Author, kind: String) {
        close(sendHangup = false)
        val callId = ByteArray(16).also(SecureRandom()::nextBytes)
            .joinToString("") { "%02x".format(it) }
        state = CallUiState(
            phase = CallPhase.CONNECTING,
            callId = callId,
            peerAccount = peer.accountId,
            peerLabel = peer.label,
            kind = if (kind == "video") "video" else "voice",
            outgoing = true,
            speakerEnabled = kind == "video",
            status = "Ringing",
        )
        CallRingback.start(appContext, callId)
        emit("invite")
        main.postDelayed(ringTimeout, 60_000)
    }

    fun accept() {
        if (state.phase != CallPhase.INCOMING) return
        clearIncomingArtifacts(state.callId)
        main.removeCallbacks(ringTimeout)
        preparePeer()
        state = state.copy(phase = CallPhase.CONNECTING, status = "Connecting")
        emit("accept")
    }

    fun reject() {
        if (state.phase == CallPhase.INCOMING) emit("reject")
        close(sendHangup = false)
    }

    fun hangup() = close(sendHangup = true)

    fun toggleMute() {
        val muted = !state.muted
        audioTrack?.setEnabled(!muted)
        audioDeviceModule.setMicrophoneMute(muted)
        state = state.copy(muted = muted)
    }

    fun toggleSpeaker() {
        val enabled = !state.speakerEnabled
        routeAudio(enabled)
        state = state.copy(speakerEnabled = enabled)
    }

    fun toggleCamera() {
        if (state.kind != "video") return
        val enabled = !state.cameraEnabled
        localVideoTrack?.setEnabled(enabled)
        state = state.copy(cameraEnabled = enabled)
    }

    fun switchCamera() {
        videoCapturer?.switchCamera(null)
    }

    fun handle(frame: JSONObject, peerLabel: String) {
        val callId = frame.optString("call_id")
        val from = frame.optString("from")
        val action = frame.optString("action")
        val kind = frame.optString("kind", "voice")
        if (action != "heartbeat" && action != "heartbeat_ack") {
            Log.i("NexusCall", "received action=$action kind=$kind phase=${state.phase}")
        }
        val payload = runCatching {
            JSONObject(String(unb64(frame.getString("payload"))))
        }.getOrElse { return }
        if (action == "invite") {
            if (state.phase == CallPhase.INCOMING && state.callId == callId &&
                state.peerAccount == from && state.kind == kind
            ) return
            if (state.phase != CallPhase.IDLE) {
                sendSignal(OutgoingCallSignal(from, callId, "reject", kind))
                return
            }
            state = CallUiState(
                phase = CallPhase.INCOMING,
                callId = callId,
                peerAccount = from,
                peerLabel = peerLabel,
                kind = kind,
                speakerEnabled = kind == "video",
                status = "Incoming ${if (kind == "video") "video" else "voice"} call",
            )
            CallRinger.start(appContext, callId)
            main.postDelayed(ringTimeout, 60_000)
            return
        }
        if (callId != state.callId || from != state.peerAccount || kind != state.kind) return
        when (action) {
            "accept" -> {
                if (!state.outgoing) return
                main.removeCallbacks(ringTimeout)
                CallRingback.stop(state.callId)
                preparePeer()
                createOffer()
            }
            "offer" -> setRemoteDescription(payload, createAnswer = true)
            "answer" -> setRemoteDescription(payload, createAnswer = false)
            "ice" -> addIce(payload)
            "heartbeat" -> {
                markPeerAlive()
                emit("heartbeat_ack")
            }
            "heartbeat_ack" -> markPeerAlive()
            "reject" -> close(false, "Call declined")
            "hangup" -> close(false, "Call ended")
        }
    }

    private fun preparePeer() {
        if (peerConnection != null) return
        CallRinger.stop(state.callId)
        configureAudio()
        audioSource = factory.createAudioSource(MediaConstraints())
        audioTrack = factory.createAudioTrack("nexus-audio", audioSource).also {
            it.setEnabled(!state.muted)
        }
        if (state.kind == "video") startVideo()
        val configuredServers = buildList {
            addAll(
                relayServers.map { server ->
                    PeerConnection.IceServer.builder(server.urls).apply {
                        if (server.username.isNotEmpty()) setUsername(server.username)
                        if (server.credential.isNotEmpty()) setPassword(server.credential)
                    }.createIceServer()
                },
            )
            addAll(
                listOf(
                PeerConnection.IceServer.builder("stun:stun.cloudflare.com:3478").createIceServer(),
                PeerConnection.IceServer.builder("stun:stun.l.google.com:19302").createIceServer(),
                ),
            )
        }
        Log.i("NexusCall", "preparing peer relayServers=${relayServers.size}")
        val config = PeerConnection.RTCConfiguration(configuredServers).apply {
            sdpSemantics = PeerConnection.SdpSemantics.UNIFIED_PLAN
            continualGatheringPolicy = PeerConnection.ContinualGatheringPolicy.GATHER_CONTINUALLY
        }
        peerConnection = factory.createPeerConnection(config, peerObserver)?.also { connection ->
            configureAudioSender(connection.addTrack(audioTrack, listOf("nexus-call")))
            localVideoTrack?.let { track ->
                videoSender = connection.addTrack(track, listOf("nexus-call")).also(::configureVideoSender)
            }
        }
        state = state.copy(localVideo = localVideoTrack)
    }

    private fun configureAudio() {
        if (!audioConfigured) {
            previousAudioMode = audioManager.mode
            previousSpeakerphone = audioManager.isSpeakerphoneOn
            audioConfigured = true
        }
        val attributes = AudioAttributes.Builder()
            .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
            .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
            .build()
        audioFocusRequest = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
            .setAudioAttributes(attributes)
            .setAcceptsDelayedFocusGain(false)
            .build()
            .also(audioManager::requestAudioFocus)
        audioManager.mode = AudioManager.MODE_IN_COMMUNICATION
        routeAudio(state.speakerEnabled)
    }

    private fun routeAudio(speaker: Boolean) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val preferredTypes = if (speaker) {
                listOf(AudioDeviceInfo.TYPE_BUILTIN_SPEAKER)
            } else {
                listOf(
                    AudioDeviceInfo.TYPE_BLE_HEADSET,
                    AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
                    AudioDeviceInfo.TYPE_WIRED_HEADSET,
                    AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
                    AudioDeviceInfo.TYPE_BUILTIN_EARPIECE,
                )
            }
            val device = preferredTypes.firstNotNullOfOrNull { type ->
                audioManager.availableCommunicationDevices.firstOrNull { it.type == type }
            }
            if (device != null) {
                audioManager.setCommunicationDevice(device)
                audioDeviceModule.setPreferredInputDevice(
                    device.takeIf { it.type != AudioDeviceInfo.TYPE_BUILTIN_SPEAKER },
                )
                Log.i("NexusCall", "audio route type=${device.type} speaker=$speaker")
            } else {
                audioManager.clearCommunicationDevice()
                Log.w("NexusCall", "no communication audio device available speaker=$speaker")
            }
        } else {
            @Suppress("DEPRECATION")
            if (speaker) {
                audioManager.stopBluetoothSco()
                audioManager.isBluetoothScoOn = false
                audioManager.isSpeakerphoneOn = true
            } else {
                audioManager.isSpeakerphoneOn = false
                audioManager.startBluetoothSco()
                audioManager.isBluetoothScoOn = true
            }
        }
    }

    private fun startVideo() {
        val enumerator = Camera2Enumerator(appContext)
        val cameraName = enumerator.deviceNames.firstOrNull(enumerator::isFrontFacing)
            ?: enumerator.deviceNames.firstOrNull()
            ?: return
        videoCapturer = enumerator.createCapturer(cameraName, null)
        videoSource = factory.createVideoSource(false)
        surfaceTextureHelper = SurfaceTextureHelper.create("NexusCallCapture", eglContext)
        videoCapturer?.initialize(surfaceTextureHelper, appContext, videoSource?.capturerObserver)
        videoCapturer?.startCapture(1280, 720, 30)
        localVideoTrack = factory.createVideoTrack("nexus-video", videoSource)
    }

    private fun configureAudioSender(sender: RtpSender) {
        val parameters = sender.parameters
        parameters.encodings.forEach { encoding ->
            encoding.minBitrateBps = AUDIO_MIN_BITRATE_BPS
            encoding.maxBitrateBps = AUDIO_MAX_BITRATE_BPS
            encoding.adaptiveAudioPacketTime = true
        }
        if (!sender.setParameters(parameters)) Log.w("NexusCall", "audio sender policy rejected")
    }

    private fun configureVideoSender(sender: RtpSender) {
        val parameters = sender.parameters
        parameters.degradationPreference = RtpParameters.DegradationPreference.BALANCED
        parameters.encodings.forEach { encoding ->
            encoding.minBitrateBps = VIDEO_MIN_BITRATE_BPS
            encoding.maxBitrateBps = VIDEO_MAX_BITRATE_BPS
            encoding.maxFramerate = VIDEO_MAX_FPS
        }
        if (!sender.setParameters(parameters)) Log.w("NexusCall", "video sender policy rejected")
    }

    private fun updateQuality(stats: Collection<org.webrtc.RTCStats>) {
        var outboundBytes = 0L
        var inboundBytes = 0L
        var outboundAudioBytes = 0L
        var inboundAudioBytes = 0L
        var packetsLost = 0L
        var jitterSeconds = 0.0
        var roundTripSeconds = 0.0
        var width = 0L
        var height = 0L
        var fps = 0.0
        var selectedLocalCandidateId: String? = null
        val relayCandidateIds = mutableSetOf<String>()
        stats.forEach { stat ->
            val members = stat.members
            when (stat.type) {
                "outbound-rtp" -> {
                    val bytes = (members["bytesSent"] as? Number)?.toLong() ?: 0L
                    if (members["kind"] == "audio" || members["mediaType"] == "audio") {
                        outboundAudioBytes += bytes
                    } else if (members["kind"] == "video" || members["mediaType"] == "video") {
                        outboundBytes += bytes
                        width = (members["frameWidth"] as? Number)?.toLong() ?: width
                        height = (members["frameHeight"] as? Number)?.toLong() ?: height
                        fps = (members["framesPerSecond"] as? Number)?.toDouble() ?: fps
                    }
                }
                "inbound-rtp" -> {
                    val bytes = (members["bytesReceived"] as? Number)?.toLong() ?: 0L
                    inboundBytes += bytes
                    if (members["kind"] == "audio" || members["mediaType"] == "audio") {
                        inboundAudioBytes += bytes
                    }
                    packetsLost += (members["packetsLost"] as? Number)?.toLong() ?: 0L
                    jitterSeconds = maxOf(
                        jitterSeconds,
                        (members["jitter"] as? Number)?.toDouble() ?: 0.0,
                    )
                }
                "candidate-pair" -> if (members["nominated"] == true && members["state"] == "succeeded") {
                    roundTripSeconds = (members["currentRoundTripTime"] as? Number)?.toDouble() ?: 0.0
                    selectedLocalCandidateId = members["localCandidateId"] as? String
                }
                "local-candidate" -> if (members["candidateType"] == "relay") relayCandidateIds += stat.id
            }
        }
        val nowMs = android.os.SystemClock.elapsedRealtime()
        val elapsedSeconds = ((nowMs - lastStatsAtMs).coerceAtLeast(1L)) / 1_000.0
        val outboundKbps = if (lastStatsAtMs == 0L) 0 else
            (((outboundBytes - lastOutboundBytes).coerceAtLeast(0L) * 8) / elapsedSeconds / 1_000).toInt()
        val inboundKbps = if (lastStatsAtMs == 0L) 0 else
            (((inboundBytes - lastInboundBytes).coerceAtLeast(0L) * 8) / elapsedSeconds / 1_000).toInt()
        val outboundAudioDelta = (outboundAudioBytes - lastOutboundAudioBytes).coerceAtLeast(0L)
        val inboundAudioDelta = (inboundAudioBytes - lastInboundAudioBytes).coerceAtLeast(0L)
        val lostSinceLastReport = (packetsLost - lastPacketsLost).coerceAtLeast(0L)
        lastOutboundBytes = outboundBytes
        lastInboundBytes = inboundBytes
        lastOutboundAudioBytes = outboundAudioBytes
        lastInboundAudioBytes = inboundAudioBytes
        lastPacketsLost = packetsLost
        lastStatsAtMs = nowMs
        val quality = when {
            roundTripSeconds > 0.5 || jitterSeconds > 0.08 || lostSinceLastReport > 20 -> "Unstable"
            roundTripSeconds > 0.25 || jitterSeconds > 0.04 || lostSinceLastReport > 5 -> "Fair"
            else -> "Good"
        }
        val media = if (state.kind == "video" && width > 0 && height > 0) {
            " · ${width}×${height} · ${fps.toInt()} fps · ↑$outboundKbps/↓$inboundKbps kbps"
        } else if (inboundKbps > 0) {
            " · $inboundKbps kbps"
        } else ""
        val route = if (selectedLocalCandidateId in relayCandidateIds) " · relay" else " · direct"
        main.post {
            if (state.phase != CallPhase.ACTIVE) return@post
            state = state.copy(quality = quality + media + route)
            stalledOutboundAudioReports = if (
                !state.muted && inboundAudioDelta > 0L && outboundAudioDelta == 0L
            ) {
                stalledOutboundAudioReports + 1
            } else {
                0
            }
            if (
                stalledOutboundAudioReports >= STALLED_AUDIO_REPORT_LIMIT &&
                nowMs - lastMediaRecoveryAtMs >= MEDIA_RECOVERY_COOLDOWN_MS
            ) {
                lastMediaRecoveryAtMs = nowMs
                stalledOutboundAudioReports = 0
                beginConnectionRecovery(
                    status = "Microphone reconnecting",
                    reason = "outbound audio stalled",
                    closeAfterGrace = false,
                    createRestartOffer = true,
                )
            }
        }
    }

    private fun createOffer() {
        peerConnection?.createOffer(localSdpObserver("offer"), MediaConstraints())
    }

    private fun createAnswer() {
        peerConnection?.createAnswer(localSdpObserver("answer"), MediaConstraints())
    }

    private fun localSdpObserver(action: String) = object : EmptySdpObserver() {
        override fun onCreateSuccess(description: SessionDescription) {
            peerConnection?.setLocalDescription(object : EmptySdpObserver() {
                override fun onSetSuccess() {
                    emit(action, JSONObject().put("type", description.type.canonicalForm())
                        .put("sdp", description.description))
                }
            }, description)
        }
    }

    private fun setRemoteDescription(payload: JSONObject, createAnswer: Boolean) {
        preparePeer()
        val type = runCatching {
            SessionDescription.Type.fromCanonicalForm(payload.getString("type"))
        }.getOrNull() ?: return
        val description = SessionDescription(type, payload.optString("sdp"))
        peerConnection?.setRemoteDescription(object : EmptySdpObserver() {
            override fun onSetSuccess() {
                pendingIce.forEach { peerConnection?.addIceCandidate(it) }
                pendingIce.clear()
                if (createAnswer) createAnswer()
            }
        }, description)
    }

    private fun addIce(payload: JSONObject) {
        val candidate = IceCandidate(
            payload.optString("mid"),
            payload.optInt("line"),
            payload.optString("candidate"),
        )
        if (peerConnection?.remoteDescription == null) pendingIce += candidate
        else peerConnection?.addIceCandidate(candidate)
    }

    private fun emit(action: String, payload: JSONObject = JSONObject()) {
        val current = state
        if (current.callId.isEmpty()) return
        if (action != "heartbeat" && action != "heartbeat_ack") {
            Log.i("NexusCall", "sending action=$action kind=${current.kind} phase=${current.phase}")
        }
        sendSignal(
            OutgoingCallSignal(
                current.peerAccount,
                current.callId,
                action,
                current.kind,
                payload,
            ),
        )
    }

    private val peerObserver = object : PeerConnection.Observer {
        override fun onIceCandidate(candidate: IceCandidate) {
            emit("ice", JSONObject().put("mid", candidate.sdpMid)
                .put("line", candidate.sdpMLineIndex).put("candidate", candidate.sdp))
        }
        override fun onConnectionChange(newState: PeerConnection.PeerConnectionState) {
            Log.i("NexusCall", "peer connection state=$newState")
            main.post {
                when (newState) {
                    PeerConnection.PeerConnectionState.CONNECTED ->
                        {
                            main.removeCallbacks(ringTimeout)
                            main.removeCallbacks(disconnectTimeout)
                            audioTrack?.setEnabled(!state.muted)
                            audioDeviceModule.setMicrophoneMute(state.muted)
                            state = state.copy(phase = CallPhase.ACTIVE, status = "Secure call")
                            startHeartbeat()
                            startStats()
                        }
                    PeerConnection.PeerConnectionState.DISCONNECTED ->
                        beginConnectionRecovery("Reconnecting", "peer disconnected")
                    PeerConnection.PeerConnectionState.FAILED ->
                        beginConnectionRecovery("Connection interrupted", "peer connection failed")
                    PeerConnection.PeerConnectionState.CLOSED -> close(false, "Call ended")
                    else -> Unit
                }
            }
        }
        override fun onTrack(transceiver: RtpTransceiver) {
            val track = transceiver.receiver.track() as? VideoTrack ?: return
            main.post { state = state.copy(remoteVideo = track) }
        }
        override fun onSignalingChange(state: PeerConnection.SignalingState) = Unit
        override fun onIceConnectionChange(state: PeerConnection.IceConnectionState) {
            Log.i("NexusCall", "ICE connection state=$state")
        }
        override fun onIceConnectionReceivingChange(receiving: Boolean) = Unit
        override fun onIceGatheringChange(state: PeerConnection.IceGatheringState) {
            Log.i("NexusCall", "ICE gathering state=$state")
        }
        override fun onIceCandidatesRemoved(candidates: Array<out IceCandidate>) = Unit
        override fun onAddStream(stream: MediaStream) = Unit
        override fun onRemoveStream(stream: MediaStream) = Unit
        override fun onDataChannel(channel: DataChannel) = Unit
        override fun onRenegotiationNeeded() = Unit
        override fun onAddTrack(receiver: RtpReceiver, streams: Array<out MediaStream>) = Unit
    }

    private fun beginConnectionRecovery(
        status: String,
        reason: String,
        closeAfterGrace: Boolean = true,
        createRestartOffer: Boolean = state.outgoing,
    ) {
        if (state.phase != CallPhase.ACTIVE && state.phase != CallPhase.CONNECTING) return
        Log.w("NexusCall", "$reason; restarting ICE and local audio")
        state = state.copy(status = status)
        audioTrack?.setEnabled(!state.muted)
        audioDeviceModule.setMicrophoneMute(state.muted)
        configureAudio()
        peerConnection?.restartIce()
        if (createRestartOffer) createOffer()
        if (closeAfterGrace) {
            main.removeCallbacks(disconnectTimeout)
            main.postDelayed(disconnectTimeout, DISCONNECT_GRACE_MS)
        }
    }

    private fun close(sendHangup: Boolean, finalStatus: String = "") {
        val closingCallId = state.callId
        main.removeCallbacks(ringTimeout)
        main.removeCallbacks(disconnectTimeout)
        main.removeCallbacks(heartbeatTick)
        main.removeCallbacks(heartbeatTimeout)
        main.removeCallbacks(statsTick)
        CallRinger.stop(state.callId)
        CallRingback.stop(state.callId)
        clearIncomingArtifacts(closingCallId)
        if (sendHangup && state.phase != CallPhase.IDLE) emit("hangup")
        peerConnection?.close()
        peerConnection?.dispose()
        peerConnection = null
        runCatching { videoCapturer?.stopCapture() }
        videoCapturer?.dispose()
        videoCapturer = null
        surfaceTextureHelper?.dispose()
        surfaceTextureHelper = null
        localVideoTrack?.dispose()
        localVideoTrack = null
        videoSource?.dispose()
        videoSource = null
        audioTrack?.dispose()
        audioTrack = null
        audioSource?.dispose()
        audioSource = null
        videoSender = null
        lastOutboundBytes = 0L
        lastInboundBytes = 0L
        lastOutboundAudioBytes = 0L
        lastInboundAudioBytes = 0L
        lastPacketsLost = 0L
        lastStatsAtMs = 0L
        stalledOutboundAudioReports = 0
        lastMediaRecoveryAtMs = 0L
        if (audioConfigured) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                audioManager.clearCommunicationDevice()
            } else {
                @Suppress("DEPRECATION")
                audioManager.stopBluetoothSco()
                @Suppress("DEPRECATION")
                audioManager.isBluetoothScoOn = false
                @Suppress("DEPRECATION")
                audioManager.isSpeakerphoneOn = previousSpeakerphone
            }
            audioDeviceModule.setPreferredInputDevice(null)
            audioFocusRequest?.let(audioManager::abandonAudioFocusRequest)
            audioFocusRequest = null
            audioManager.mode = previousAudioMode
            audioConfigured = false
        }
        pendingIce.clear()
        state = CallUiState(status = finalStatus)
    }

    private fun startHeartbeat() {
        main.removeCallbacks(heartbeatTick)
        main.removeCallbacks(heartbeatTimeout)
        main.post(heartbeatTick)
    }

    private fun startStats() {
        main.removeCallbacks(statsTick)
        main.post(statsTick)
    }

    private fun markPeerAlive() {
        if (state.phase != CallPhase.ACTIVE && state.phase != CallPhase.CONNECTING) return
        main.removeCallbacks(heartbeatTimeout)
        main.postDelayed(heartbeatTimeout, HEARTBEAT_TIMEOUT_MS)
        if (state.status == "Call signaling interrupted") {
            state = state.copy(status = "Secure call")
        }
    }

    private fun clearIncomingArtifacts(callId: String) {
        if (callId.isEmpty()) return
        Store(appContext).clearPendingCall(callId)
        ConnectNotifications.cancelIncomingCall(appContext, callId)
    }

    fun dispose() {
        close(sendHangup = false)
        factory.dispose()
        audioDeviceModule.release()
        eglBase.release()
    }

    companion object {
        private const val HEARTBEAT_INTERVAL_MS = 3_000L
        private const val HEARTBEAT_TIMEOUT_MS = 30_000L
        private const val DISCONNECT_GRACE_MS = 30_000L
        private const val STATS_INTERVAL_MS = 2_000L
        private const val STALLED_AUDIO_REPORT_LIMIT = 3
        private const val MEDIA_RECOVERY_COOLDOWN_MS = 20_000L
        private const val AUDIO_MIN_BITRATE_BPS = 24_000
        private const val AUDIO_MAX_BITRATE_BPS = 64_000
        private const val VIDEO_MIN_BITRATE_BPS = 350_000
        private const val VIDEO_MAX_BITRATE_BPS = 2_500_000
        private const val VIDEO_MAX_FPS = 30
    }
}

private open class EmptySdpObserver : SdpObserver {
    override fun onCreateSuccess(description: SessionDescription) = Unit
    override fun onSetSuccess() = Unit
    override fun onCreateFailure(error: String) {
        Log.e("NexusCall", "SDP creation failed: $error")
    }
    override fun onSetFailure(error: String) {
        Log.e("NexusCall", "SDP apply failed: $error")
    }
}