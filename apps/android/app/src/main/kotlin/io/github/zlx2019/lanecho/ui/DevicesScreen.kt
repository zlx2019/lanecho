package io.github.zlx2019.lanecho.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import io.github.zlx2019.lanecho.R
import io.github.zlx2019.lanecho.core.transport.SessionException
import io.github.zlx2019.lanecho.state.AppState

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DevicesScreen(state: AppState, modifier: Modifier = Modifier) {
    var pairingWith by remember { mutableStateOf<String?>(null) }

    val paired = remember(state.peersVersion) { state.engine.paired.all() }
    val online = remember(state.peersVersion) {
        state.engine.registry.snapshot().associateBy { it.info.fingerprint }
    }
    val nearby = remember(state.peersVersion) {
        state.engine.registry.snapshot().filter { !state.engine.paired.isPaired(it.info.fingerprint) }
    }

    Column(modifier.fillMaxSize()) {
        TopAppBar(title = { Text(stringResource(R.string.tab_devices)) })
        LazyColumn(Modifier.fillMaxSize()) {
            item { SectionHeader(stringResource(R.string.devices_paired)) }
            items(paired, key = { "paired-" + it.fingerprint }) { peer ->
                val isOnline = peer.fingerprint in online
                ListItem(
                    headlineContent = { Text(peer.name) },
                    supportingContent = {
                        val platform = online[peer.fingerprint]?.info?.platform ?: ""
                        val status = stringResource(
                            if (isOnline) R.string.devices_online else R.string.devices_offline,
                        )
                        Text(listOf(platform, status).filter { it.isNotEmpty() }.joinToString(" · "))
                    },
                    leadingContent = { Text(if (isOnline) "●" else "○") },
                    trailingContent = {
                        TextButton(onClick = { state.unpair(peer.fingerprint) }) {
                            Text(stringResource(R.string.devices_unpair))
                        }
                    },
                )
            }

            item { SectionHeader(stringResource(R.string.devices_nearby)) }
            items(nearby, key = { "nearby-" + it.info.fingerprint }) { peer ->
                val busy = pairingWith == peer.info.fingerprint
                ListItem(
                    headlineContent = { Text(peer.info.name) },
                    supportingContent = {
                        Text(if (busy) stringResource(R.string.devices_pairing) else peer.info.platform)
                    },
                    leadingContent = { Text("⊕") },
                    trailingContent = {
                        if (busy) CircularProgressIndicator(Modifier.size(20.dp))
                    },
                    modifier = Modifier.clickable(enabled = !busy) {
                        pairingWith = peer.info.fingerprint
                        state.pairWith(peer.info.fingerprint) { result ->
                            pairingWith = null
                            result.exceptionOrNull()?.let { error ->
                                state.showToast(pairErrorText(state, error))
                            }
                        }
                    },
                )
            }

            item {
                Text(
                    stringResource(R.string.devices_hint),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(16.dp),
                )
            }
        }
    }
}

@Composable
private fun SectionHeader(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

private fun pairErrorText(state: AppState, error: Throwable): String {
    val context = state.appContext
    return when (error) {
        is SessionException.PairRefused -> context.getString(R.string.devices_pair_refused)
        else -> context.getString(R.string.devices_pair_failed, error.message ?: error.javaClass.simpleName)
    }
}
