package dev.assist.app

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import uniffi.assist_ffi.Listener
import uniffi.assist_ffi.ModelFile
import uniffi.assist_ffi.installModel
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL

/** Sample rate the core expects. */
private const val RATE = 16_000

/** Samples per push: 100 ms. */
private const val CHUNK = RATE / 10

/** Downloads a voice model and has the core check and install it. Blocking: call off the main thread. */
fun download(model: ModelFile, dataDir: File, cacheDir: File) {
    val part = File(cacheDir, "${model.name}.part")
    // A little over the published size; the core's checksum catches anything else.
    val limit = (model.sizeMb.toLong() + 2) * 1_200_000
    val connection = URL(model.url).openConnection() as HttpURLConnection
    try {
        connection.connectTimeout = 15_000
        connection.readTimeout = 30_000
        if (connection.responseCode != HttpURLConnection.HTTP_OK) {
            throw IOException("${model.name}: HTTP ${connection.responseCode}")
        }
        connection.inputStream.use { input ->
            part.outputStream().use { output ->
                val buffer = ByteArray(64 * 1024)
                var total = 0L
                while (true) {
                    val n = input.read(buffer)
                    if (n < 0) break
                    total += n
                    if (total > limit) throw IOException("${model.name} is larger than expected")
                    output.write(buffer, 0, n)
                }
            }
        }
    } catch (e: IOException) {
        part.delete()
        throw e
    } finally {
        connection.disconnect()
    }
    installModel(dataDir.absolutePath, model.name, part.absolutePath)
}

/**
 * Records the microphone until the core hears one utterance and returns its text (empty when
 * nothing was understood). Stops after [WAIT_MS] without speech or when the caller's
 * coroutine is cancelled. Needs the microphone permission.
 */
@SuppressLint("MissingPermission")
suspend fun listenOnce(listener: Listener): String {
    listener.reset()
    val size = maxOf(AudioRecord.getMinBufferSize(RATE, MONO, FLOAT), CHUNK * 4 * 4)
    val recorder = AudioRecord(MediaRecorder.AudioSource.VOICE_RECOGNITION, RATE, MONO, FLOAT, size)
    if (recorder.state != AudioRecord.STATE_INITIALIZED) {
        recorder.release()
        throw IOException("The microphone is not available.")
    }
    try {
        recorder.startRecording()
        val chunk = FloatArray(CHUNK)
        var silentMs = 0
        while (true) {
            currentCoroutineContext().ensureActive()
            val n = recorder.read(chunk, 0, CHUNK, AudioRecord.READ_BLOCKING)
            if (n < 0) throw IOException("The microphone stopped ($n).")
            listener.push(chunk.copyOf(n).toList())?.let { return it }
            silentMs = if (listener.isSpeaking()) 0 else silentMs + n * 1000 / RATE
            if (silentMs > WAIT_MS) return ""
        }
    } finally {
        recorder.stop()
        recorder.release()
    }
}

private const val MONO = AudioFormat.CHANNEL_IN_MONO
private const val FLOAT = AudioFormat.ENCODING_PCM_FLOAT

/** How long to wait for speech after the mic is tapped. */
private const val WAIT_MS = 8_000
