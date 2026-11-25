package dev.assist.app

import android.Manifest
import android.app.Application
import android.content.pm.PackageManager
import android.speech.tts.TextToSpeech
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.assist_ffi.AssistException
import uniffi.assist_ffi.Assistant
import uniffi.assist_ffi.Job
import uniffi.assist_ffi.Listener
import uniffi.assist_ffi.ModelFile
import uniffi.assist_ffi.missingModels
import java.io.IOException
import kotlinx.coroutines.Job as Task

/** One line of the conversation. */
data class Line(val fromUser: Boolean, val text: String)

/** Holds the Rust core while the screen lives, and the conversation shown on it. */
class ChatViewModel(private val app: Application) : AndroidViewModel(app) {
    /** The conversation, oldest first, bounded to [MAX_LINES]. */
    val lines = mutableStateListOf<Line>()

    /** Android permissions to ask the user for; the screen shows the system prompt. */
    val permissions = Channel<String>(Channel.CONFLATED)

    /** The mic is recording. */
    var listening by mutableStateOf(false)
        private set

    /** Voice models waiting for the user's consent to download; empty when none. */
    var offer by mutableStateOf<List<ModelFile>>(emptyList())
        private set

    private val phone = AndroidPhone(app) { permissions.trySend(it) }
    private val speech = TextToSpeech(app) {}
    private var assistant: Assistant? = null
    private var listener: Listener? = null
    private var recording: Task? = null
    private var downloading = false

    init {
        if (app.checkSelfPermission(CONTACTS) != PackageManager.PERMISSION_GRANTED) {
            show(false, "Allow contacts so I can call and text people by name.")
            permissions.trySend(CONTACTS)
        }
        start()
    }

    /** Called with the user's answer to a permission prompt. */
    fun permissionAnswered(permission: String, granted: Boolean) {
        // The contact list is read when the core starts.
        if (granted && permission == CONTACTS) start()
        if (granted && permission == MIC) talk()
    }

    /** Sends what the user typed or said to the core and shows its answer; speaks it if [spoken]. */
    fun send(text: String, spoken: Boolean = false) {
        val core = assistant ?: return
        show(true, text)
        viewModelScope.launch {
            val answer = withContext(Dispatchers.IO) { core.say(text) }
            answer.say.forEach { show(false, it) }
            if (spoken && answer.say.isNotEmpty()) {
                speech.speak(answer.say.joinToString(" "), TextToSpeech.QUEUE_FLUSH, null, "reply")
            }
            answer.jobs.forEach { launch { follow(core, it) } }
        }
    }

    /** Push-to-talk: starts listening for one request, or stops if it is listening. */
    fun talk() {
        recording?.let { it.cancel(); return }
        if (downloading) return
        if (app.checkSelfPermission(MIC) != PackageManager.PERMISSION_GRANTED) {
            permissions.trySend(MIC)
            return
        }
        val missing = missingModels(app.filesDir.absolutePath)
        if (missing.isNotEmpty()) {
            offer = missing
            return
        }
        speech.stop()
        recording = viewModelScope.launch {
            listening = true
            try {
                val heard = withContext(Dispatchers.IO) {
                    val ears = listener ?: Listener(app.filesDir.absolutePath).also { listener = it }
                    listenOnce(ears)
                }
                if (heard.isNotEmpty()) send(heard, spoken = true) else show(false, "I didn't hear anything.")
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

    /** The user's answer to the model download offer. */
    fun answerOffer(accepted: Boolean) {
        val models = offer
        offer = emptyList()
        if (!accepted) return
        downloading = true
        viewModelScope.launch {
            show(false, "Downloading the voice models (${models.sumOf { it.sizeMb.toInt() }} MB).")
            try {
                withContext(Dispatchers.IO) { models.forEach { download(it, app.filesDir, app.cacheDir) } }
                show(false, "Voice is ready. Tap Talk and speak.")
            } catch (e: IOException) {
                show(false, "Download failed: ${e.message}")
            } catch (e: AssistException) {
                show(false, "Download failed: ${e.message}")
            } finally {
                downloading = false
            }
        }
    }

    /** Starts the core, or restarts it to pick up the phone's contacts and apps. */
    private fun start() {
        viewModelScope.launch {
            val old = assistant
            assistant = null
            // Opening the core reads its data directory and the phone's lists: off the main thread.
            val opened = withContext(Dispatchers.IO) {
                old?.close()
                runCatching {
                    Assistant(
                        "",
                        app.filesDir.absolutePath,
                        PhoneLists.contacts(app),
                        PhoneLists.apps(app),
                        phone,
                    )
                }
            }
            opened.onSuccess { assistant = it }
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

    private fun show(fromUser: Boolean, text: String) {
        lines.add(Line(fromUser, text))
        if (lines.size > MAX_LINES) lines.removeRange(0, lines.size - MAX_LINES)
    }

    override fun onCleared() {
        // Writes pending job history.
        assistant?.close()
        listener?.close()
        speech.shutdown()
    }

    private companion object {
        const val CONTACTS = Manifest.permission.READ_CONTACTS
        const val MIC = Manifest.permission.RECORD_AUDIO
        const val MAX_LINES = 200
        const val POLL_MS = 200L
    }
}
