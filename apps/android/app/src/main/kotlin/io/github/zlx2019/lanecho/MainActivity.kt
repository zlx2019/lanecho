package io.github.zlx2019.lanecho

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import io.github.zlx2019.lanecho.core.history.HistoryEntry
import io.github.zlx2019.lanecho.state.AppState
import io.github.zlx2019.lanecho.ui.DetailScreen
import io.github.zlx2019.lanecho.ui.DevicesScreen
import io.github.zlx2019.lanecho.ui.HistoryScreen
import io.github.zlx2019.lanecho.ui.LanechoTheme
import io.github.zlx2019.lanecho.ui.SettingsScreen

class MainActivity : ComponentActivity() {

    private val state: AppState get() = (application as LanechoApplication).appState

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            LanechoTheme {
                MainScaffold(state)
            }
        }
    }

    override fun onStart() {
        super.onStart()
        state.onForeground()
    }

    override fun onStop() {
        super.onStop()
        state.onBackground()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        // Clipboard reads need window focus on Android 10+, so the DA3
        // opportunistic upstream hangs off this hook rather than onStart
        if (hasFocus) state.onFocused()
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MainScaffold(state: AppState) {
    var tab by remember { mutableIntStateOf(0) }
    var detailEntryId by remember { mutableStateOf<String?>(null) }
    val snackbar = remember { SnackbarHostState() }

    // Received-content banner (DA2): snackbar with a copy action; auto-write
    // mode only reports what already happened
    val banner = state.banner
    val context = androidx.compose.ui.platform.LocalContext.current
    val copyLabel = stringResource(R.string.action_copy)
    LaunchedEffect(banner) {
        val current = banner ?: return@LaunchedEffect
        val isImage = current.entry.kind == io.github.zlx2019.lanecho.core.history.EntryKind.IMAGE
        val text = if (isImage) {
            context.getString(R.string.banner_received_image, current.fromName)
        } else {
            context.getString(R.string.banner_received_text, current.fromName)
        }
        val result = if (current.autoWritten) {
            snackbar.showSnackbar(message = text)
            SnackbarResult.Dismissed
        } else {
            snackbar.showSnackbar(message = text, actionLabel = copyLabel)
        }
        if (result == SnackbarResult.ActionPerformed) state.copyEntry(current.entry)
        state.banner = null
    }
    val toast = state.toast
    LaunchedEffect(toast) {
        val message = toast ?: return@LaunchedEffect
        snackbar.showSnackbar(message)
        state.toast = null
    }

    // Inbound pairing dialog, reachable from any tab (DA6)
    state.pairRequest?.let { remote ->
        AlertDialog(
            onDismissRequest = { state.resolvePair(remote.fingerprint, false) },
            title = { Text(stringResource(R.string.pair_request_title)) },
            text = {
                Text(
                    stringResource(R.string.pair_request_body, remote.name, remote.platform) + "\n" +
                        stringResource(
                            R.string.pair_request_fingerprint,
                            remote.fingerprint.take(8) + "…" + remote.fingerprint.takeLast(4),
                        ) + "\n" +
                        stringResource(R.string.pair_request_check),
                )
            },
            confirmButton = {
                TextButton(onClick = { state.resolvePair(remote.fingerprint, true) }) {
                    Text(stringResource(R.string.pair_accept))
                }
            },
            dismissButton = {
                TextButton(onClick = { state.resolvePair(remote.fingerprint, false) }) {
                    Text(stringResource(R.string.pair_refuse))
                }
            },
        )
    }

    val detailId = detailEntryId
    if (detailId != null) {
        BackHandler { detailEntryId = null }
        DetailScreen(state, detailId, onClose = { detailEntryId = null })
        return
    }

    Scaffold(
        snackbarHost = { SnackbarHost(snackbar) },
        bottomBar = {
            NavigationBar {
                NavigationBarItem(
                    selected = tab == 0, onClick = { tab = 0 },
                    icon = { Icon(Icons.Filled.ContentPaste, contentDescription = null) },
                    label = { Text(stringResource(R.string.tab_history)) },
                )
                NavigationBarItem(
                    selected = tab == 1, onClick = { tab = 1 },
                    icon = { Icon(Icons.Filled.Devices, contentDescription = null) },
                    label = { Text(stringResource(R.string.tab_devices)) },
                )
                NavigationBarItem(
                    selected = tab == 2, onClick = { tab = 2 },
                    icon = { Icon(Icons.Filled.Settings, contentDescription = null) },
                    label = { Text(stringResource(R.string.tab_settings)) },
                )
            }
        },
    ) { padding ->
        val content = Modifier.padding(padding)
        when (tab) {
            0 -> HistoryScreen(
                state = state,
                modifier = content,
                onOpenDetail = { detailEntryId = it },
                onGoPair = { tab = 1 },
            )
            1 -> DevicesScreen(state = state, modifier = content)
            else -> SettingsScreen(state = state, modifier = content)
        }
    }
}

/** Detail entry lookup used by the detail screen. */
fun AppState.findEntry(id: String): HistoryEntry? = engine.history.find(id)
