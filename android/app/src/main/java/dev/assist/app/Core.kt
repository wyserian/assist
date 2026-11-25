package dev.assist.app

import android.Manifest
import android.app.Activity
import android.app.Application
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.os.Build as Device
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.speech.tts.TextToSpeech
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.unifiedpush.android.connector.UnifiedPush
import uniffi.assist_ffi.AssistException
import uniffi.assist_ffi.Assistant
import uniffi.assist_ffi.Feature
import uniffi.assist_ffi.Job
import uniffi.assist_ffi.Listener
import uniffi.assist_ffi.ModelFile
import uniffi.assist_ffi.missingModels
import java.io.File
import java.io.IOException
import kotlinx.coroutines.Job as Task

/** One line of the conversation. */
data class Line(val fromUser: Boolean, val text: String)

/** The app owns one core for its whole process, shared by the chat screen and the mic service. */
class AssistApp : Application() {
    lateinit var core: Core
        private set

    override fun onCreate() {
        super.onCreate()
        core = Core(this)
    }
}

/**
 * The Rust core and the conversation around it: typed input, push-to-talk and the wake word
 * from [HandsFreeService], spoken replies, model downloads and the permissions they need.
 */
class Core(private val app: Application) {
    /** The conversation, oldest first, bounded to [MAX_LINES]. */
    val lines = mutableStateListOf<Line>()

    /** Android permissions to ask the user for; the screen shows the system prompt. */
    val permissions = Channel<String>(Channel.CONFLATED)

    /** Screen recording waits for the user's OK; the screen shows Android's capture prompt. */
    val screenConsents = Channel<Unit>(Channel.CONFLATED)

    /** Push-to-talk is recording. */
    var listening by mutableStateOf(false)
        private set

    /** The wake-word service runs. */
    var handsFree by mutableStateOf(false)
        private set

    /** A request heard after the wake word waits for Yes or No on screen. */
    var confirming by mutableStateOf(false)
        private set

    /** The paired computer's name, or null before pairing. */
    var computer by mutableStateOf<String?>(null)
        private set

    /** Typed and push-to-talk requests go to the paired computer instead of this phone. */
    var useComputer by mutableStateOf(false)

    /** The pairing dialog is open. */
    var linking by mutableStateOf(false)

    /** Voice models waiting for the user's consent to download; empty when none. */
    var offer by mutableStateOf<List<ModelFile>>(emptyList())
        private set

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val phone = AndroidPhone(
        app,
        ask = { permissions.trySend(it) },
        offer = ::offerModels,
        askScreen = { screenConsents.trySend(Unit) },
        tell = { text, speak -> scope.launch { notice(text, speak) } },
        showLine = { text -> scope.launch { show(false, text) } },
    )
    private val facts = PhoneFacts(app) { name, value -> assistant?.setFact(name, value) }
    private var notices = 0
    private val speech = TextToSpeech(app) {}
    private var assistant: Assistant? = null
    private var listener: Listener? = null
    private var recording: Task? = null
    private var downloading = false

    /** Asks [keepLinked] to connect now rather than at its next check. */
    private val wakeups = Channel<Unit>(Channel.CONFLATED)

    /** This phone's push address, handed to the core once it runs; "" forgets it. */
    private var push: String? = null
    private var pushHinted = false

    /** What to start again once the microphone permission is granted. */
    private var afterMic: (() -> Unit)? = null

    /** How a request reached the core. */
    private enum class Via { TYPED, TALK, WAKE }

    init {
        if (!granted(CONTACTS)) {
            show(false, "Allow contacts so I can call and text people by name.")
            permissions.trySend(CONTACTS)
        }
        facts.start()
        start()
        keepLinked()
    }

    /**
     * Keeps the connection to the paired computer open while the app runs, so the computer
     * can run phone commands here ("text mom" said to it). Unreachable is not an error.
     */
    private fun keepLinked() {
        scope.launch {
            while (true) {
                val core = assistant
                if (core != null && computer != null) {
                    withContext(Dispatchers.IO) { runCatching { core.connectComputer() } }
                }
                withTimeoutOrNull(LINK_CHECK_MS) { wakeups.receive() }
            }
        }
    }

    /** Connects to the paired computer now: it woke this phone for a request. */
    fun linkNow() {
        wakeups.trySend(Unit)
    }

    /**
     * Registers for wake-ups once paired, so the computer can reach the phone with the app
     * closed. Needs a UnifiedPush distributor app (ntfy); without one the link works while
     * the app runs.
     */
    fun registerPush(activity: Activity) {
        if (computer == null) return
        UnifiedPush.tryUseCurrentOrDefaultDistributor(activity) { found ->
            if (found) {
                UnifiedPush.register(app)
            } else if (!pushHinted) {
                pushHinted = true
                scope.launch { show(false, "Install the ntfy app so the computer can reach this phone while Assist is closed.") }
            }
        }
    }

    /** The distributor gave this phone a push address ("" when it took it back). */
    fun pushAddress(url: String) {
        scope.launch {
            push = url
            sendPush()
        }
    }

    /** Registering for wake-ups failed. */
    fun pushFailed(reason: String) {
        scope.launch { show(false, "Wake-ups from the computer are off: $reason.") }
    }

    /** Hands the push address to the core, which tells the computer on the next connection. */
    private suspend fun sendPush() {
        val core = assistant ?: return
        val url = push ?: return
        val saved = withContext(Dispatchers.IO) { runCatching { core.setPush(url) } }
        if (saved.isSuccess) linkNow()
    }

    /** Called with the user's answer to a permission prompt. */
    fun permissionAnswered(permission: String, granted: Boolean) {
        // The contact list is read when the core starts.
        if (granted && permission == CONTACTS) start()
        if (permission == MIC) afterMic?.takeIf { granted }?.invoke()
    }

    /** Pairs with the computer whose code (`assist pair`) is [code]. */
    fun pair(code: String) {
        val core = assistant ?: return
        linking = false
        show(false, "Linking to the computer.")
        scope.launch {
            val result = withContext(Dispatchers.IO) {
                runCatching { core.pairWith(code, Device.MODEL) }
            }
            result.onSuccess {
                computer = it
                useComputer = true
                show(false, "Linked to $it. What you type now goes there; switch it off for this phone.")
            }
            result.onFailure { err ->
                if (err !is AssistException) throw err
                show(false, "Not linked: ${err.message}")
            }
        }
    }

    /** Sends what the user typed. */
    fun send(text: String) = converse(text, Via.TYPED)

    /** A request heard after the wake word, from [HandsFreeService]. */
    fun heard(text: String) = converse(text, Via.WAKE)

    /** The user tapped Yes or No for a request heard after the wake word. */
    fun confirmSpoken(yes: Boolean) {
        confirming = false
        converse(if (yes) "yes" else "no", Via.WAKE, vouched = true)
    }

    /** The wake word came alone: answer so the user knows it is listening. */
    fun woke() {
        val core = assistant ?: return
        val line = core.wokeReply()
        show(false, line)
        speak(listOf(line))
    }

    /** Stops a job, such as a recording from its notification. */
    fun stopJob(job: ULong) {
        assistant?.stop(job)
    }

    /** Stops a reply being spoken (barge-in). */
    fun stopSpeaking() {
        speech.stop()
    }

    /** A reply is being spoken now. */
    fun speaking(): Boolean = speech.isSpeaking

    /** How long requests need no wake word, for [HandsFreeService]. */
    fun awakeSeconds(): UInt = assistant?.awakeSeconds() ?: DEFAULT_AWAKE_S

    /** The wake phrases for [HandsFreeService]; empty until the core has started. */
    fun wakePhrases(): List<String> = assistant?.wakePhrases().orEmpty()

    /** Push-to-talk: starts listening for one request, or stops if it is listening. */
    fun talk() {
        recording?.let { it.cancel(); return }
        if (downloading || !ready(Feature.TALK, ::talk)) return
        speech.stop()
        recording = scope.launch {
            listening = true
            try {
                val heard = withContext(Dispatchers.IO) {
                    val ears = listener ?: Listener(dataDir()).also { listener = it }
                    listenOnce(ears)
                }
                if (heard.isNotEmpty()) converse(heard, Via.TALK) else show(false, "I didn't hear anything.")
            } catch (e: AssistException) {
                show(false, "Voice: ${e.message}")
            } catch (e: IOException) {
                show(false, e.message ?: "The microphone failed.")
            } finally {
                listening = false
                recording = null
            }
        }
    }

    /** Turns the wake-word service on or off. */
    fun toggleHandsFree() {
        if (handsFree) {
            app.stopService(Intent(app, HandsFreeService::class.java))
            return
        }
        if (downloading || !ready(Feature.HANDS_FREE, ::toggleHandsFree)) return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && !granted(NOTIFICATIONS)) {
            // The service runs without it, but its notification is the way to stop it.
            permissions.trySend(NOTIFICATIONS)
        }
        app.startForegroundService(Intent(app, HandsFreeService::class.java))
    }

    /** Called by [HandsFreeService] when it starts and stops. */
    fun handsFreeChanged(on: Boolean, error: String? = null) {
        handsFree = on
        error?.let { show(false, it) }
    }

    /** Offers the models [feature] still needs; called from the core's threads. */
    private fun offerModels(feature: Feature) {
        scope.launch { if (!downloading) offer = missingModels(dataDir(), feature) }
    }

    /** The user's answer to the model download offer. */
    fun answerOffer(accepted: Boolean) {
        val models = offer
        offer = emptyList()
        if (!accepted) return
        downloading = true
        scope.launch {
            show(false, "Downloading the models (${models.sumOf { it.sizeMb.toInt() }} MB).")
            try {
                withContext(Dispatchers.IO) { models.forEach { download(it, app.filesDir, app.cacheDir) } }
                show(false, "Done. Ask again.")
            } catch (e: IOException) {
                show(false, "Download failed: ${e.message}")
            } catch (e: AssistException) {
                show(false, "Download failed: ${e.message}")
            } finally {
                downloading = false
            }
        }
    }

    /** Whether voice can start: asks for the microphone (then runs [retry]) or offers the missing models if not. */
    private fun ready(feature: Feature, retry: () -> Unit): Boolean {
        if (!granted(MIC)) {
            afterMic = retry
            permissions.trySend(MIC)
            return false
        }
        val missing = missingModels(dataDir(), feature)
        if (missing.isNotEmpty()) {
            offer = missing
            return false
        }
        return true
    }

    private fun converse(text: String, via: Via, vouched: Boolean = false) {
        val core = assistant ?: return
        show(true, text)
        if (useComputer && via != Via.WAKE) {
            remote(core, text, spoken = via == Via.TALK)
            return
        }
        scope.launch {
            val answer = withContext(Dispatchers.IO) {
                if (via == Via.WAKE) core.hear(text, vouched) else core.say(text)
            }
            answer.say.forEach { show(false, it) }
            if (via != Via.TYPED) speak(answer.say)
            if (via == Via.WAKE) confirming = answer.waiting
            answer.jobs.forEach { launch { follow(core, it) } }
        }
    }

    /** Says [text] to the paired computer and shows its answer. */
    private fun remote(core: Assistant, text: String, spoken: Boolean) {
        scope.launch {
            val answer = withContext(Dispatchers.IO) { runCatching { core.sayRemote(text) } }
            answer.onSuccess {
                it.say.forEach { line -> show(false, line) }
                if (spoken) speak(it.say)
            }
            answer.onFailure { err ->
                if (err !is AssistException) throw err
                show(false, "The computer: ${err.message}")
            }
        }
    }

    private fun speak(lines: List<String>) {
        if (lines.isNotEmpty()) speech.speak(lines.joinToString(" "), TextToSpeech.QUEUE_FLUSH, null, "reply")
    }

    /** Starts the core, or restarts it to pick up the phone's contacts and apps. */
    private fun start() {
        scope.launch {
            val old = assistant
            assistant = null
            // Opening the core reads its data directory and the phone's lists: off the main thread.
            val opened = withContext(Dispatchers.IO) {
                old?.close()
                runCatching {
                    Assistant(config(), dataDir(), PhoneLists.contacts(app), PhoneLists.apps(app), phone)
                }
            }
            opened.onSuccess {
                assistant = it
                computer = it.pairedComputer()
                facts.replay()
                sendPush()
                linkNow()
            }
            opened.onFailure { err ->
                if (err !is AssistException) throw err
                show(false, "Cannot start: ${err.message}")
            }
        }
    }

    // shortcut: shows a job's output once it ends, stream lines when long jobs reach the phone.
    private suspend fun follow(core: Assistant, job: Job) {
        // A restarted core has closed this one.
        while (core === assistant) {
            val out = withContext(Dispatchers.IO) { core.output(job.id) }
            if (out.ended) {
                out.lines.forEach { show(false, it) }
                return
            }
            delay(POLL_MS)
        }
    }

    /** A proactive message: shown in the chat, then spoken or posted as a notification. */
    private fun notice(text: String, speak: Boolean) {
        show(false, text)
        if (speak) {
            speak(listOf(text))
            return
        }
        val manager = app.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(NOTICES, "Messages", NotificationManager.IMPORTANCE_DEFAULT))
        val shown = Notification.Builder(app, NOTICES)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle("Assist")
            .setContentText(text)
            .build()
        // Notices replace each other after a few, so they cannot pile up.
        manager.notify(NOTICE_BASE + notices++ % MAX_NOTICES, shown)
    }

    /** The owner's `config.toml` in the app's files (same format as the desktop), if any. */
    private fun config(): String {
        val file = File(app.filesDir, "config.toml")
        if (!file.isFile) return ""
        if (file.length() > MAX_CONFIG) throw AssistException.Start("config.toml is larger than $MAX_CONFIG bytes")
        return runCatching { file.readText() }.getOrElse { throw AssistException.Start("cannot read config.toml: ${it.message}") }
    }

    private fun show(fromUser: Boolean, text: String) {
        lines.add(Line(fromUser, text))
        if (lines.size > MAX_LINES) lines.removeRange(0, lines.size - MAX_LINES)
    }

    private fun granted(permission: String) =
        app.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED

    private fun dataDir() = app.filesDir.absolutePath

    private companion object {
        const val CONTACTS = Manifest.permission.READ_CONTACTS
        const val MIC = Manifest.permission.RECORD_AUDIO
        const val NOTIFICATIONS = "android.permission.POST_NOTIFICATIONS"
        const val NOTICES = "notices"
        const val NOTICE_BASE = 100
        const val MAX_NOTICES = 5
        const val MAX_CONFIG = 64 * 1024
        const val MAX_LINES = 200
        const val DEFAULT_AWAKE_S = 60u
        const val LINK_CHECK_MS = 15_000L
        const val POLL_MS = 200L
    }
}
