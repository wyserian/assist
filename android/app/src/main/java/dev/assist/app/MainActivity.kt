package dev.assist.app

import android.content.Intent
import android.media.projection.MediaProjectionManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.TextButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp

/** The chat screen: type or talk to the assistant, read its answers. */
class MainActivity : ComponentActivity() {
    private val core get() = (application as AssistApp).core

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            val colors = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()
            MaterialTheme(colorScheme = colors) {
                Surface(Modifier.fillMaxSize()) { Chat(core) }
            }
        }
        if (savedInstanceState == null) assist(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        assist(intent)
    }

    /** Opened as the phone's assistant (long press on home, assistant gesture): start listening. */
    private fun assist(intent: Intent?) {
        if (intent?.action == Intent.ACTION_ASSIST && !core.listening) core.talk()
    }
}

@Composable
private fun Chat(model: Core) {
    var input by rememberSaveable { mutableStateOf("") }
    val list = rememberLazyListState()
    val prompt = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { answers ->
        answers.forEach { (permission, granted) -> model.permissionAnswered(permission, granted) }
    }
    LaunchedEffect(Unit) {
        for (permission in model.permissions) prompt.launch(arrayOf(permission))
    }
    val capture = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
        Recordings.consentAnswered(result.resultCode, result.data)
    }
    val context = LocalContext.current
    LaunchedEffect(Unit) {
        for (ask in model.screenConsents) {
            capture.launch(context.getSystemService(MediaProjectionManager::class.java).createScreenCaptureIntent())
        }
    }
    LaunchedEffect(model.lines.size) {
        if (model.lines.isNotEmpty()) list.animateScrollToItem(model.lines.lastIndex)
    }
    val send = {
        val text = input.trim()
        if (text.isNotEmpty()) {
            model.send(text)
            input = ""
        }
    }
    Column(Modifier.fillMaxSize().safeDrawingPadding().imePadding().padding(12.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            Text("Listen for the wake word", Modifier.weight(1f))
            Switch(checked = model.handsFree, onCheckedChange = { model.toggleHandsFree() })
        }
        LazyColumn(
            Modifier.weight(1f).fillMaxWidth(),
            state = list,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            items(model.lines) { Bubble(it) }
        }
        if (model.confirming) {
            Row(Modifier.padding(bottom = 8.dp)) {
                Button(onClick = { model.confirmSpoken(true) }) { Text("Yes") }
                OutlinedButton(onClick = { model.confirmSpoken(false) }, Modifier.padding(start = 8.dp)) { Text("No") }
            }
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = input,
                onValueChange = { input = it.take(MAX_INPUT) },
                modifier = Modifier.weight(1f),
                placeholder = { Text("Say something") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { send() }),
            )
            Button(onClick = send, modifier = Modifier.padding(start = 8.dp)) { Text("Send") }
            FilledTonalButton(onClick = model::talk, modifier = Modifier.padding(start = 8.dp)) {
                Text(if (model.listening) "Stop" else "Talk")
            }
        }
    }
    if (model.offer.isNotEmpty()) {
        val files = model.offer.joinToString("\n") { "${it.name}: ${it.sizeMb} MB, licence ${it.licence}" }
        AlertDialog(
            onDismissRequest = { model.answerOffer(false) },
            title = { Text("Download models?") },
            text = { Text("Assist does this on the phone, offline, with these models:\n\n$files") },
            confirmButton = { TextButton(onClick = { model.answerOffer(true) }) { Text("Download") } },
            dismissButton = { TextButton(onClick = { model.answerOffer(false) }) { Text("Not now") } },
        )
    }
}

@Composable
private fun Bubble(line: Line) {
    val colors = MaterialTheme.colorScheme
    val side = if (line.fromUser) Alignment.CenterEnd else Alignment.CenterStart
    Box(Modifier.fillMaxWidth(), contentAlignment = side) {
        Surface(
            color = if (line.fromUser) colors.primaryContainer else colors.surfaceVariant,
            shape = MaterialTheme.shapes.medium,
            modifier = Modifier.widthIn(max = 320.dp),
        ) {
            Text(line.text, Modifier.padding(horizontal = 12.dp, vertical = 8.dp))
        }
    }
}

/** Longest line the user can type. */
private const val MAX_INPUT = 500
