package dev.assist.app

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.ContentValues
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.media.AudioManager
import android.net.Uri
import android.os.Build
import android.provider.ContactsContract
import android.provider.MediaStore
import android.provider.Settings
import android.telephony.SmsManager
import uniffi.assist_ffi.App
import uniffi.assist_ffi.Contact
import uniffi.assist_ffi.Feature
import uniffi.assist_ffi.Phone
import uniffi.assist_ffi.PhoneException
import uniffi.assist_ffi.Recording
import java.io.File
import kotlin.math.roundToInt

/**
 * What the core asks the phone to do. Policy and confirmation already happened in the core;
 * this only checks the Android permission, asking for it through [ask] when it is missing,
 * shows the model download offer through [offer], the screen capture prompt through
 * [askScreen] and proactive messages through [tell].
 */
class AndroidPhone(
    private val context: Context,
    private val ask: (String) -> Unit,
    private val offer: (Feature) -> Unit,
    private val askScreen: () -> Unit,
    private val tell: (String, Boolean) -> Unit,
) : Phone {
    override fun openApp(packageName: String) {
        val intent = context.packageManager.getLaunchIntentForPackage(packageName)
            ?: throw PhoneException.Failed("That app is no longer installed.")
        context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    override fun call(number: String) {
        need(Manifest.permission.CALL_PHONE, "calls")
        val intent = Intent(Intent.ACTION_CALL, Uri.fromParts("tel", number, null))
        context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }

    override fun sendSms(number: String, text: String) {
        need(Manifest.permission.SEND_SMS, "text messages")
        val sms = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            context.getSystemService(SmsManager::class.java)
        } else {
            @Suppress("DEPRECATION")
            SmsManager.getDefault()
        } ?: throw PhoneException.Failed("This phone cannot send text messages.")
        sms.sendMultipartTextMessage(number, null, sms.divideMessage(text), null, null)
    }

    override fun setVolume(percent: UByte) {
        val audio = context.getSystemService(AudioManager::class.java)
        val max = audio.getStreamMaxVolume(AudioManager.STREAM_MUSIC)
        val level = (percent.toInt() * max / 100.0).roundToInt()
        audio.setStreamVolume(AudioManager.STREAM_MUSIC, level, AudioManager.FLAG_SHOW_UI)
    }

    override fun lock() {
        if (!accessibility().lock()) throw PhoneException.Failed("Android did not lock the screen.")
    }

    override fun screenshot(name: String): String {
        val picture = shot()
        val values = ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, name)
            put(MediaStore.Images.Media.MIME_TYPE, "image/png")
            put(MediaStore.Images.Media.RELATIVE_PATH, GALLERY)
        }
        val resolver = context.contentResolver
        val uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)
            ?: throw PhoneException.Failed("The gallery did not take the screenshot.")
        resolver.openOutputStream(uri)?.use { picture.compress(Bitmap.CompressFormat.PNG, 100, it) }
            ?: throw PhoneException.Failed("The gallery did not take the screenshot.")
        return "$GALLERY/$name"
    }

    override fun capture(): String {
        val file = File(context.cacheDir, "screen-${System.nanoTime()}.png")
        file.outputStream().use { shot().compress(Bitmap.CompressFormat.PNG, 100, it) }
        return file.absolutePath
    }

    override fun offerModels(feature: Feature) = offer(feature)

    override fun startRecording(what: Recording, name: String, job: ULong): String {
        if (what == Recording.AUDIO) need(Manifest.permission.RECORD_AUDIO, "the microphone")
        return Recordings.start(context, what, name, job, askScreen)
    }

    override fun stopRecording(): String = Recordings.stop()

    override fun notice(text: String, speak: Boolean) = tell(text, speak)

    private fun shot(): Bitmap = accessibility().capture().getOrElse {
        throw PhoneException.Failed(it.message ?: "No screenshot.")
    }

    /** The accessibility service; opens its settings and fails if the user has not turned it on. */
    private fun accessibility(): AssistAccessibility = AssistAccessibility.current ?: run {
        context.startActivity(Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        throw PhoneException.Failed("Turn on Assist in Accessibility settings, then ask again.")
    }

    private fun need(permission: String, what: String) {
        if (context.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED) return
        ask(permission)
        throw PhoneException.Failed("Allow $what for Assist, then ask again.")
    }
}

/** Where screenshots go: the gallery's Pictures folder. */
private const val GALLERY = "Pictures/Assist"

/** The phone's contacts and launcher apps, for the core to match names against. */
object PhoneLists {
    /** Contacts with a number, primary number first; empty without the contacts permission. */
    fun contacts(context: Context): List<Contact> {
        if (context.checkSelfPermission(Manifest.permission.READ_CONTACTS) != PackageManager.PERMISSION_GRANTED) {
            return emptyList()
        }
        val columns = arrayOf(
            ContactsContract.CommonDataKinds.Phone.DISPLAY_NAME,
            ContactsContract.CommonDataKinds.Phone.NUMBER,
        )
        val order = "${ContactsContract.CommonDataKinds.Phone.IS_SUPER_PRIMARY} DESC"
        val found = LinkedHashMap<String, String>()
        context.contentResolver.query(
            ContactsContract.CommonDataKinds.Phone.CONTENT_URI, columns, null, null, order,
        )?.use { rows ->
            while (rows.moveToNext()) {
                val name = rows.getString(0) ?: continue
                val number = rows.getString(1) ?: continue
                found.putIfAbsent(name, number)
            }
        }
        return found.map { (name, number) -> Contact(name, number) }
    }

    /** Apps on the launcher, by label, without this one. */
    fun apps(context: Context): List<App> {
        val pm = context.packageManager
        val launcher = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        return pm.queryIntentActivities(launcher, 0)
            .filter { it.activityInfo.packageName != context.packageName }
            .map { App(it.loadLabel(pm).toString(), it.activityInfo.packageName) }
    }
}
