package dev.assist.app

import android.app.KeyguardManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.os.BatteryManager
import uniffi.assist_ffi.FactValue

/**
 * Keeps the phone's facts current while the app's process runs: `node.battery`,
 * `node.charging` (from the battery broadcast) and `screen.locked`. Each change goes to
 * [report]; the latest values are kept so a restarted core gets them again.
 */
class PhoneFacts(private val context: Context, private val report: (String, FactValue) -> Unit) {
    private val latest = LinkedHashMap<String, FactValue>()

    /** Starts listening; the battery broadcast is sticky, so its facts arrive at once. */
    fun start() {
        context.registerReceiver(battery, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
        context.registerReceiver(
            screen,
            IntentFilter().apply {
                addAction(Intent.ACTION_SCREEN_OFF)
                addAction(Intent.ACTION_SCREEN_ON)
                addAction(Intent.ACTION_USER_PRESENT)
            },
        )
        update(LOCKED, FactValue.Bool(locked()))
    }

    /** Sends every known fact again, e.g. to a core that just started. */
    @Synchronized
    fun replay() = latest.forEach { (name, value) -> report(name, value) }

    @Synchronized
    private fun update(name: String, value: FactValue) {
        if (latest[name] == value) return
        latest[name] = value
        report(name, value)
    }

    private fun locked() = context.getSystemService(KeyguardManager::class.java).isKeyguardLocked

    private val battery = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            val level = intent.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
            val scale = intent.getIntExtra(BatteryManager.EXTRA_SCALE, -1)
            if (level >= 0 && scale > 0) update(BATTERY, FactValue.Number(level * 100.0 / scale))
            val plugged = intent.getIntExtra(BatteryManager.EXTRA_PLUGGED, 0) != 0
            update(CHARGING, FactValue.Bool(plugged))
        }
    }

    private val screen = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            // Screen off may lock only after a delay; the keyguard says what is true now.
            update(LOCKED, FactValue.Bool(intent.action != Intent.ACTION_USER_PRESENT && locked()))
        }
    }

    private companion object {
        const val BATTERY = "node.battery"
        const val CHARGING = "node.charging"
        const val LOCKED = "screen.locked"
    }
}
