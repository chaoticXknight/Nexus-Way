package com.nexusway.connect

import android.content.Context
import android.media.MediaRecorder
import android.os.Build
import java.io.File

class VoiceNoteRecorder(private val context: Context) {
    private var recorder: MediaRecorder? = null
    private var output: File? = null

    fun start() {
        if (recorder != null) return
        val directory = File(context.cacheDir, "wire-recordings").apply { mkdirs() }
        val file = File.createTempFile("voice_", ".m4a", directory)
        val mediaRecorder = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            MediaRecorder(context)
        } else {
            @Suppress("DEPRECATION")
            MediaRecorder()
        }
        mediaRecorder.apply {
            setAudioSource(MediaRecorder.AudioSource.VOICE_COMMUNICATION)
            setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            setAudioChannels(1)
            setAudioSamplingRate(48_000)
            setAudioEncodingBitRate(64_000)
            setMaxDuration(VOICE_NOTE_MAX_DURATION_MS)
            setOutputFile(file.absolutePath)
            prepare()
            start()
        }
        output = file
        recorder = mediaRecorder
    }

    fun stop(): File? {
        val mediaRecorder = recorder ?: return null
        recorder = null
        val file = output
        output = null
        val completed = runCatching { mediaRecorder.stop() }.isSuccess
        mediaRecorder.reset()
        mediaRecorder.release()
        if (!completed) file?.delete()
        return file?.takeIf { completed && it.length() > 0L }
    }

    fun cancel() {
        stop()?.delete()
    }

    companion object {
        private const val VOICE_NOTE_MAX_DURATION_MS = 5 * 60 * 1_000
    }
}