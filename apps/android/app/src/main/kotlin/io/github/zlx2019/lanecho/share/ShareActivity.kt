package io.github.zlx2019.lanecho.share

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.zlx2019.lanecho.LanechoApplication
import io.github.zlx2019.lanecho.R
import io.github.zlx2019.lanecho.core.discovery.Peer
import io.github.zlx2019.lanecho.core.transport.DialTarget
import io.github.zlx2019.lanecho.core.transport.SessionException
import io.github.zlx2019.lanecho.core.transport.Sessions
import io.github.zlx2019.lanecho.ui.LanechoTheme
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

/** Per-target delivery outcome shown live in the sheet. */
private data class TargetState(val peer: Peer, val status: Status, val detail: String = "") {
    enum class Status { SENDING, SENT, REFUSED, UNREACHABLE }
}

// The share-sheet entry (ACTION_SEND text/plain): comes up over the calling
// app, dials every paired online peer (desktop broadcast semantics, no
// per-target picking, decision in PLAN §9.5), then closes itself
class ShareActivity : ComponentActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val app = application as LanechoApplication
        val text = if (intent?.action == Intent.ACTION_SEND && intent.type == "text/plain") {
            intent.getStringExtra(Intent.EXTRA_TEXT)
        } else {
            null
        }
        if (text.isNullOrEmpty()) {
            finish()
            return
        }
        setContent {
            LanechoTheme {
                ShareSheet(app, text, onDone = { finish() })
            }
        }
    }

    override fun onStart() {
        super.onStart()
        (application as LanechoApplication).appState.onForeground()
    }

    override fun onStop() {
        super.onStop()
        (application as LanechoApplication).appState.onBackground()
    }
}

@Composable
private fun ShareSheet(app: LanechoApplication, text: String, onDone: () -> Unit) {
    val engine = app.engine
    val targets = remember { mutableStateListOf<TargetState>() }
    var phase by remember { mutableStateOf("searching") } // searching | sending | done | none
    var attempt by remember { mutableIntStateOf(0) }

    LaunchedEffect(attempt) {
        if (engine.paired.all().isEmpty()) {
            phase = "no-paired"
            return@LaunchedEffect
        }
        phase = "searching"
        targets.clear()
        // Cold start: give discovery a moment to find the peers (mDNS ~1-3s)
        val found = withContext(Dispatchers.IO) {
            val deadline = System.currentTimeMillis() + 4_000
            var peers = engine.onlinePairedPeers()
            while (peers.isEmpty() && System.currentTimeMillis() < deadline) {
                delay(200)
                peers = engine.onlinePairedPeers()
            }
            peers
        }
        if (found.isEmpty()) {
            phase = "none"
            return@LaunchedEffect
        }
        phase = "sending"
        targets.addAll(found.map { TargetState(it, TargetState.Status.SENDING) })
        val timestamp = System.currentTimeMillis()
        withContext(Dispatchers.IO) {
            engine.history.recordText(text, origin = null, timestampMs = timestamp)
            found.forEachIndexed { index, peer ->
                val outcome = try {
                    Sessions.syncText(
                        engine.identity, engine.localInfo(),
                        DialTarget(peer.addresses, peer.port, peer.info.fingerprint),
                        seq = 0, timestampMs = timestamp, text = text,
                    )
                    TargetState(peer, TargetState.Status.SENT)
                } catch (e: SessionException.Rejected) {
                    TargetState(peer, TargetState.Status.REFUSED, e.reasonCode)
                } catch (_: Exception) {
                    TargetState(peer, TargetState.Status.UNREACHABLE)
                }
                withContext(Dispatchers.Main) { targets[index] = outcome }
            }
        }
        phase = "done"
        // Everything delivered: close on our own after a beat
        if (targets.all { it.status == TargetState.Status.SENT }) {
            delay(1_500)
            onDone()
        }
    }

    Column(
        Modifier
            .fillMaxWidth()
            .padding(16.dp),
        verticalArrangement = androidx.compose.foundation.layout.Arrangement.Bottom,
    ) {
        Card(Modifier.fillMaxWidth()) {
            Column(Modifier.padding(16.dp)) {
                Text(stringResource(R.string.share_title), style = MaterialTheme.typography.titleMedium)
                Card(
                    Modifier
                        .fillMaxWidth()
                        .padding(vertical = 12.dp),
                ) {
                    Text(
                        text, maxLines = 2, overflow = TextOverflow.Ellipsis,
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(12.dp),
                    )
                }
                when (phase) {
                    "searching" -> StatusLine(stringResource(R.string.share_searching), busy = true)
                    "no-paired" -> StatusLine(stringResource(R.string.share_no_paired), busy = false)
                    "none" -> StatusLine(stringResource(R.string.share_no_devices), busy = false)
                    else -> targets.forEach { target ->
                        Row(
                            Modifier
                                .fillMaxWidth()
                                .padding(vertical = 4.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            val (symbol, label) = when (target.status) {
                                TargetState.Status.SENDING -> "…" to stringResource(R.string.share_sending)
                                TargetState.Status.SENT -> "✓" to stringResource(R.string.share_sent)
                                TargetState.Status.REFUSED -> "✗" to stringResource(R.string.share_refused, target.detail)
                                TargetState.Status.UNREACHABLE -> "✗" to stringResource(R.string.share_unreachable)
                            }
                            Text(symbol, Modifier.padding(end = 8.dp))
                            Text(target.peer.info.name, Modifier.weight(1f), maxLines = 1)
                            Text(label, style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
                Row(Modifier.fillMaxWidth(), horizontalArrangement = androidx.compose.foundation.layout.Arrangement.End) {
                    if (phase == "none") {
                        TextButton(onClick = { attempt++ }) { Text(stringResource(R.string.share_retry)) }
                    }
                    TextButton(onClick = onDone) { Text(stringResource(R.string.share_done)) }
                }
            }
        }
    }
}

@Composable
private fun StatusLine(text: String, busy: Boolean) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.padding(vertical = 8.dp)) {
        if (busy) CircularProgressIndicator(Modifier.size(18.dp))
        Text(text, Modifier.padding(start = if (busy) 10.dp else 0.dp), style = MaterialTheme.typography.bodyMedium)
    }
}
