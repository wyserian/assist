package dev.assist.app

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioManager
import android.net.Uri
import android.os.Build
import android.provider.ContactsContract
import android.telephony.SmsManager
import uniffi.assist_ffi.App
import uniffi.assist_ffi.Contact
import uniffi.assist_ffi.Phone
import uniffi.assist_ffi.PhoneException
import kotlin.math.roundToInt

/**
 * What the core asks the phone to do. Policy and confirmation already happened in the core;
 * this only checks the Android permission, asking for it through [ask] when it is missing.
 */
class AndroidPhone(private val context: Context, private val ask: (String) -> Unit) : Phone {
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

    private fun need(permission: String, what: String) {
        if (context.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED) return
        ask(permission)
        throw PhoneException.Failed("Allow $what for Assist, then ask again.")
    }
}

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
