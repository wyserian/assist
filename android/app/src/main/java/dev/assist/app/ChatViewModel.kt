package dev.assist.app

import android.Manifest
import android.app.Application
import android.content.pm.PackageManager
import androidx.compose.runtime.mutableStateListOf
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

/** One line of the conversation. */
data class Line(val fromUser: Boolean, val text: String)

/** Holds the Rust core while the screen lives, and the conversation shown on it. */
class ChatViewModel(private val app: Application) : AndroidViewModel(app) {
    /** The conversation, oldest first, bounded to [MAX_LINES]. */
    val lines = mutableStateListOf<Line>()

    /** Android permissions to ask the user for; the screen shows the system prompt. */
    val permissions = Channel<String>(Channel.CONFLATED)

    private val phone = AndroidPhone(app) { permissions.trySend(it) }
    private var assistant: Assistant? = null

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
    }

    /** Sends what the user typed to the core and shows its answer. */
    fun send(text: String) {
        val core = assistant ?: return
        show(true, text)
        viewModelScope.launch {
            val answer = withContext(Dispatchers.IO) { core.say(text) }
            answer.say.forEach { show(false, it) }
            answer.jobs.forEach { launch { follow(core, it) } }
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
    }

    private companion object {
        const val CONTACTS = Manifest.permission.READ_CONTACTS
        const val MAX_LINES = 200
        const val POLL_MS = 200L
    }
}
