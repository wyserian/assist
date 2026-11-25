package dev.assist.app

import android.accessibilityservice.AccessibilityService
import android.graphics.Bitmap
import android.os.Build
import android.view.Display
import android.view.accessibility.AccessibilityEvent
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * Locks the screen and takes screenshots when the user asks. It subscribes to no events and
 * reads no window content; the user turns it on in Accessibility settings.
 */
class AssistAccessibility : AccessibilityService() {
    override fun onServiceConnected() {
        current = this
    }

    override fun onDestroy() {
        current = null
        super.onDestroy()
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {}

    override fun onInterrupt() {}

    /** Locks the screen; false if Android refused. */
    fun lock(): Boolean = performGlobalAction(GLOBAL_ACTION_LOCK_SCREEN)

    /** The screen as it is now, or why not. Blocks up to a few seconds. */
    fun capture(): Result<Bitmap> {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            return Result.failure(IllegalStateException("Screenshots need Android 11 or later."))
        }
        val done = CountDownLatch(1)
        var result: Result<Bitmap> = Result.failure(IllegalStateException("The screenshot timed out."))
        takeScreenshot(
            Display.DEFAULT_DISPLAY,
            worker,
            object : TakeScreenshotCallback {
                override fun onSuccess(screenshot: ScreenshotResult) {
                    val buffer = screenshot.hardwareBuffer
                    val bitmap = Bitmap.wrapHardwareBuffer(buffer, screenshot.colorSpace)
                    // A software copy, so it can be compressed after the buffer is closed.
                    result = bitmap?.copy(Bitmap.Config.ARGB_8888, false)
                        ?.let { Result.success(it) }
                        ?: Result.failure(IllegalStateException("The screenshot was empty."))
                    buffer.close()
                    done.countDown()
                }

                override fun onFailure(errorCode: Int) {
                    result = Result.failure(IllegalStateException("Android refused the screenshot ($errorCode)."))
                    done.countDown()
                }
            },
        )
        done.await(TIMEOUT_S, TimeUnit.SECONDS)
        return result
    }

    companion object {
        /** The running service, or null when the user has not turned it on. */
        @Volatile
        var current: AssistAccessibility? = null
            private set

        private val worker = Executors.newSingleThreadExecutor()
        private const val TIMEOUT_S = 5L
    }
}
