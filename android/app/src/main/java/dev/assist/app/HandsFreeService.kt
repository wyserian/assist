package dev.assist.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.assist_ffi.AssistException
import uniffi.assist_ffi.HandsFree
import uniffi.assist_ffi.Hearing
import java.io.IOException

/**
 * Listens for the wake word while it runs: a foreground service of type microphone, with a
 * notification that says so and stops it. What it hears goes to the shared [Core].
 */
class HandsFreeService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var listening: kotlinx.coroutines.Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        } else {
            startForeground(NOTIFICATION, notification())
        }
        val core = (application as AssistApp).core
        core.handsFreeChanged(true)
        if (listening?.isActive != true) listening = scope.launch { listen(core) }
        // A restart without the user would record without them knowing; they turn it on again.
        return START_NOT_STICKY
    }

    private suspend fun listen(core: Core) {
        try {
            // The core may still be starting when the service does.
            var wake = core.wakePhrases()
            while (wake.isEmpty()) {
                delay(200)
                wake = core.wakePhrases()
            }
            val ears = withContext(Dispatchers.IO) { HandsFree(filesDir.absolutePath, wake, core.awakeSeconds()) }
            withContext(Dispatchers.IO) {
                record<Unit> { chunk ->
                    val heard = ears.push(chunk, core.speaking())
                    if (heard.isNotEmpty()) scope.launch { heard.forEach { act(core, it) } }
                    null
                }
            }
        } catch (e: AssistException) {
            core.handsFreeChanged(false, "Voice: ${e.message}")
            stopSelf()
        } catch (e: IOException) {
            core.handsFreeChanged(false, e.message ?: "The microphone failed.")
            stopSelf()
        }
    }

    private fun act(core: Core, heard: Hearing) = when (heard) {
        Hearing.Interrupt -> core.stopSpeaking()
        Hearing.Woke -> core.woke()
        is Hearing.Text -> core.heard(heard.text)
    }

    override fun onDestroy() {
        scope.cancel()
        (application as AssistApp).core.handsFreeChanged(false)
        super.onDestroy()
    }

    private fun notification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, "Listening for the wake word", NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, HandsFreeService::class.java).setAction(STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle("Assist is listening for the wake word")
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, "Stop", stop).build())
            .setOngoing(true)
            .build()
    }

    private companion object {
        const val CHANNEL = "hands-free"
        const val NOTIFICATION = 1
        const val STOP = "dev.assist.app.STOP"
    }
}
