package dev.assist.app

import android.app.Application
import androidx.compose.runtime.mutableStateListOf
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.assist_ffi.AssistException
import uniffi.assist_ffi.Assistant
import uniffi.assist_ffi.Job

/** One line of the conversation. */
data class Line(val fromUser: Boolean, val text: String)

/** Holds the Rust core while the screen lives, and the conversation shown on it. */
class ChatViewModel(app: Application) : AndroidViewModel(app) {
    /** The conversation, oldest first, bounded to [MAX_LINES]. */
    val lines = mutableStateListOf<Line>()

    private var assistant: Assistant? = null

    init {
        viewModelScope.launch {
            // Opening the core reads its data directory: off the main thread.
            val opened = withContext(Dispatchers.IO) {
                runCatching { Assistant("", app.filesDir.absolutePath) }
            }
            opened.onSuccess { assistant = it }
            opened.onFailure { err ->
                if (err !is AssistException) throw err
                show(false, "Cannot start: ${err.message}")
            }
        }
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

    // shortcut: shows a job's output once it ends, stream lines when long jobs reach the phone.
    private suspend fun follow(core: Assistant, job: Job) {
        while (true) {
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
        const val MAX_LINES = 200
        const val POLL_MS = 200L
    }
}
