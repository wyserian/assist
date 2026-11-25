package dev.assist.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import org.unifiedpush.android.connector.FailedReason
import org.unifiedpush.android.connector.PushService
import org.unifiedpush.android.connector.data.PushEndpoint
import org.unifiedpush.android.connector.data.PushMessage

/**
 * Wake-ups through UnifiedPush, with the ntfy app as the distributor: the computer posts to
 * this phone's push address when a request waits for it, and the phone connects. The message
 * carries nothing; the request itself comes over the authenticated link.
 */
class PushWake : PushService() {
    private val core get() = (application as AssistApp).core

    override fun onNewEndpoint(endpoint: PushEndpoint, instance: String) = core.pushAddress(endpoint.url)

    override fun onMessage(message: PushMessage, instance: String) {
        // The distributor raises the app to the foreground for a few seconds, enough to start
        // a short foreground service that keeps it up while the request runs.
        val held = runCatching { startForegroundService(Intent(this, LinkService::class.java)) }
        if (held.isFailure) core.linkNow()
    }

    override fun onRegistrationFailed(reason: FailedReason, instance: String) = core.pushFailed(reason.name)

    override fun onUnregistered(instance: String) = core.pushAddress("")
}

/**
 * Keeps the app in the foreground for a short while after a wake-up, so a request the
 * computer queued for the phone runs with the app closed.
 */
class LinkService : Service() {
    private val handler = Handler(Looper.getMainLooper())

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(NOTIFICATION, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_SHORT_SERVICE)
        } else {
            startForeground(NOTIFICATION, notification())
        }
        (application as AssistApp).core.linkNow()
        handler.removeCallbacksAndMessages(null)
        handler.postDelayed({ stopSelf() }, HOLD_MS)
        return START_NOT_STICKY
    }

    override fun onTimeout(startId: Int, fgsType: Int) = stopSelf()

    override fun onDestroy() {
        handler.removeCallbacksAndMessages(null)
        super.onDestroy()
    }

    private fun notification(): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "Requests from the computer", NotificationManager.IMPORTANCE_LOW))
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("Assist")
            .setContentText("Running a request from the computer.")
            .build()
    }

    private companion object {
        const val CHANNEL = "link"
        const val NOTIFICATION = 3
        // A short foreground service may run 3 minutes; the computer waits 2 for a result.
        const val HOLD_MS = 150_000L
    }
}
