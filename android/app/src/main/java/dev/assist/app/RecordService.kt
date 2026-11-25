package dev.assist.app

import android.app.Activity
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaRecorder
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import uniffi.assist_ffi.PhoneException
import uniffi.assist_ffi.Recording
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Hands recordings between the core's blocking calls ([start], [stop], from its threads) and
 * [RecordService], which records in the foreground with a notification. One at a time.
 */
object Recordings {
    /** A recording to start. */
    class Request(val what: Recording, val name: String, val job: ULong, val consent: Intent?)

    @Volatile
    internal var request: Request? = null
    internal val started = LinkedBlockingQueue<Result<String>>(1)

    /** The user's answer to the screen capture prompt; null when declined. */
    private val consent = LinkedBlockingQueue<Result<Intent>>(1)

    /** Called by the screen with the capture prompt's result. */
    fun consentAnswered(code: Int, data: Intent?) {
        consent.clear()
        consent.offer(if (code == Activity.RESULT_OK && data != null) Result.success(data) else Result.failure(Exception()))
    }

    /** Starts recording; for the screen, [ask] shows the capture prompt first. Returns where it goes. */
    fun start(context: Context, what: Recording, name: String, job: ULong, ask: () -> Unit): String {
        if (RecordService.current != null) fail("Already recording; stop that job first.")
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) fail("Recording needs Android 10 or later.")
        val allowed = if (what == Recording.SCREEN) {
            consent.clear()
            ask()
            consent.poll(CONSENT_S, TimeUnit.SECONDS)?.getOrNull()
                ?: fail("Screen recording needs your OK in Assist.")
        } else {
            null
        }
        started.clear()
        request = Request(what, name, job, allowed)
        context.startForegroundService(Intent(context, RecordService::class.java))
        return started.poll(START_S, TimeUnit.SECONDS)?.getOrElse { fail(it.message ?: "Recording did not start.") }
            ?: fail("Recording did not start.")
    }

    /** Stops and saves; returns where it went. */
    fun stop(): String = RecordService.current?.finish() ?: fail("Nothing is recording.")

    private fun fail(reason: String): Nothing = throw PhoneException.Failed(reason)

    private const val CONSENT_S = 60L
    private const val START_S = 10L
}

/** Records the screen or the microphone into the gallery while its notification shows. */
class RecordService : Service() {
    private var recorder: MediaRecorder? = null
    private var projection: MediaProjection? = null
    private var display: VirtualDisplay? = null
    private var file: ParcelFileDescriptor? = null
    private var uri: Uri? = null
    private var where = ""
    private var job: ULong = 0u

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == STOP) {
            // The core stops the job, which stops this through `Recordings.stop`.
            (application as AssistApp).core.stopJob(job)
            return START_NOT_STICKY
        }
        val request = Recordings.request ?: run {
            stopSelf()
            return START_NOT_STICKY
        }
        Recordings.request = null
        job = request.job
        val result = runCatching {
            val screen = request.what == Recording.SCREEN
            val type = if (screen) ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION else ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE
            startForeground(NOTIFICATION, notification(screen), type)
            current = this
            begin(request)
        }
        result.onFailure { cleanUp(save = false) }
        Recordings.started.offer(result)
        return START_NOT_STICKY
    }

    private fun begin(request: Recordings.Request): String {
        val screen = request.what == Recording.SCREEN
        val (collection, folder, mime) = if (screen) {
            Triple(MediaStore.Video.Media.EXTERNAL_CONTENT_URI, "Movies/Assist", "video/mp4")
        } else {
            Triple(MediaStore.Audio.Media.EXTERNAL_CONTENT_URI, "Music/Assist", "audio/mp4")
        }
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, request.name)
            put(MediaStore.MediaColumns.MIME_TYPE, mime)
            put(MediaStore.MediaColumns.RELATIVE_PATH, folder)
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val target = contentResolver.insert(collection, values) ?: error("The gallery did not take the recording.")
        uri = target
        val output = contentResolver.openFileDescriptor(target, "w") ?: error("The gallery did not take the recording.")
        file = output
        val media = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) MediaRecorder(this) else @Suppress("DEPRECATION") MediaRecorder()
        recorder = media
        if (screen) {
            val (width, height, density) = screenSize()
            media.setVideoSource(MediaRecorder.VideoSource.SURFACE)
            media.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            media.setVideoEncoder(MediaRecorder.VideoEncoder.H264)
            media.setVideoSize(width, height)
            media.setVideoFrameRate(FPS)
            media.setVideoEncodingBitRate(VIDEO_BPS)
            media.setOutputFile(output.fileDescriptor)
            media.prepare()
            val manager = getSystemService(MediaProjectionManager::class.java)
            val consent = request.consent ?: error("Screen recording was not allowed.")
            val capture = manager.getMediaProjection(Activity.RESULT_OK, consent) ?: error("Screen recording was not allowed.")
            projection = capture
            capture.registerCallback(object : MediaProjection.Callback() {
                override fun onStop() {
                    // Android or the user ended the capture: stop the job too.
                    (application as AssistApp).core.stopJob(job)
                }
            }, Handler(Looper.getMainLooper()))
            display = capture.createVirtualDisplay(
                "assist-record", width, height, density,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, media.surface, null, null,
            )
        } else {
            media.setAudioSource(MediaRecorder.AudioSource.MIC)
            media.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            media.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            media.setAudioSamplingRate(AUDIO_RATE)
            media.setAudioEncodingBitRate(AUDIO_BPS)
            media.setOutputFile(output.fileDescriptor)
            media.prepare()
        }
        media.start()
        where = "$folder/${request.name}"
        return where
    }

    /** Stops and saves; returns where it went. Called from the core's thread. */
    fun finish(): String {
        val saved = cleanUp(save = true)
        return if (saved) where else throw PhoneException.Failed("The recording was too short and was not saved.")
    }

    /** Releases everything; keeps the file only if [save] and it has content. Returns whether it was kept. */
    @Synchronized
    private fun cleanUp(save: Boolean): Boolean {
        current = null
        // MediaRecorder throws on stop when nothing was recorded yet.
        val recorded = runCatching { recorder?.stop() }.isSuccess && recorder != null
        recorder?.release()
        recorder = null
        display?.release()
        display = null
        projection?.stop()
        projection = null
        file?.close()
        file = null
        val keep = save && recorded
        uri?.let { target ->
            if (keep) {
                contentResolver.update(target, ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }, null, null)
            } else {
                contentResolver.delete(target, null, null)
            }
        }
        uri = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
        return keep
    }

    override fun onDestroy() {
        if (current === this) cleanUp(save = true)
        super.onDestroy()
    }

    /** The screen size, scaled down to at most [MAX_WIDTH] wide, in even pixels the encoder takes. */
    private fun screenSize(): Triple<Int, Int, Int> {
        val metrics = resources.displayMetrics
        val scale = minOf(1.0, MAX_WIDTH.toDouble() / metrics.widthPixels)
        val even = { n: Double -> (n.toInt() / 16) * 16 }
        return Triple(even(metrics.widthPixels * scale), even(metrics.heightPixels * scale), metrics.densityDpi)
    }

    private fun notification(screen: Boolean): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "Recording", NotificationManager.IMPORTANCE_LOW))
        val stop = PendingIntent.getService(
            this, 2, Intent(this, RecordService::class.java).setAction(STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle(if (screen) "Assist is recording the screen" else "Assist is recording audio")
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .setOngoing(true)
            .build()
    }

    companion object {
        /** The recording in progress, if any. */
        @Volatile
        var current: RecordService? = null
            private set

        private const val CHANNEL = "recording"
        private const val NOTIFICATION = 2
        private const val STOP = "dev.assist.app.STOP_RECORDING"
        private const val MAX_WIDTH = 720
        private const val FPS = 30
        private const val VIDEO_BPS = 4_000_000
        private const val AUDIO_RATE = 44_100
        private const val AUDIO_BPS = 128_000
    }
}
