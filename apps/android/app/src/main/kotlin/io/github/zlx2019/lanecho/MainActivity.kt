package io.github.zlx2019.lanecho

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.slideInHorizontally
import androidx.compose.animation.slideOutHorizontally
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.Devices
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.outlined.ContentPaste
import androidx.compose.material.icons.outlined.Devices
import androidx.compose.material.icons.outlined.Settings
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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import io.github.zlx2019.lanecho.core.history.HistoryEntry
import io.github.zlx2019.lanecho.state.AppState
import io.github.zlx2019.lanecho.ui.DetailScreen
import io.github.zlx2019.lanecho.ui.DevicesScreen
import io.github.zlx2019.lanecho.ui.HistoryScreen
import io.github.zlx2019.lanecho.ui.LanechoTheme
import io.github.zlx2019.lanecho.ui.SettingsScreen
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {

    private val state: AppState get() = (application as LanechoApplication).appState

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        // UI-review sample data, opt-in from adb only (see seedDemoData docs)
        // Debug-build-only hooks: the launcher activity is exported, so a
        // release build must not take peer or history injections from intents
        if (debuggable()) {
            if (intent?.getBooleanExtra("seed-demo", false) == true) state.seedDemoData()
            intent?.getStringExtra("probe-peer")?.let { state.injectProbePeer(it) }
        }
        // Background arrivals surface as notifications (Android 13+ runtime
        // permission); a refusal only mutes them, the service still runs
        if (android.os.Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) !=
            android.content.pm.PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
        }
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

    private fun debuggable(): Boolean =
        applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE != 0
}

@Composable
private fun MainScaffold(state: AppState) {
    // Pager state is the single source of truth for the selected tab: swipes
    // and bottom-bar taps both land on it
    val pagerState = rememberPagerState(pageCount = { 3 })
    val scope = rememberCoroutineScope()
    var detailEntryId by remember { mutableStateOf<String?>(null) }
    val snackbar = remember { SnackbarHostState() }
    fun goTab(index: Int) {
        scope.launch { pagerState.animateScrollToPage(index) }
    }

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

    BackHandler(enabled = detailEntryId != null) { detailEntryId = null }

    // Detail slides in over the tabs and slides back out on return
    AnimatedContent(
        targetState = detailEntryId,
        transitionSpec = {
            if (targetState != null) {
                (slideInHorizontally { it / 3 } + fadeIn()) togetherWith fadeOut()
            } else {
                fadeIn() togetherWith (slideOutHorizontally { it / 3 } + fadeOut())
            }
        },
        label = "detail",
    ) { entryId ->
        if (entryId != null) {
            DetailScreen(state, entryId, onClose = { detailEntryId = null })
        } else {
            // Top inset is owned by each tab's own TopAppBar; zeroing the
            // scaffold insets avoids double status-bar padding (title sag)
            Scaffold(
                contentWindowInsets = androidx.compose.foundation.layout.WindowInsets(0.dp),
                snackbarHost = { SnackbarHost(snackbar) },
                bottomBar = {
                    NavigationBar {
                        TabItem(pagerState.currentPage == 0, { goTab(0) }, Icons.Filled.ContentPaste, Icons.Outlined.ContentPaste, R.string.tab_history)
                        TabItem(pagerState.currentPage == 1, { goTab(1) }, Icons.Filled.Devices, Icons.Outlined.Devices, R.string.tab_devices)
                        TabItem(pagerState.currentPage == 2, { goTab(2) }, Icons.Filled.Settings, Icons.Outlined.Settings, R.string.tab_settings)
                    }
                },
            ) { padding ->
                HorizontalPager(
                    state = pagerState,
                    modifier = Modifier.padding(padding),
                ) { page ->
                    when (page) {
                        0 -> HistoryScreen(
                            state = state,
                            onOpenDetail = { detailEntryId = it },
                            onGoPair = { goTab(1) },
                        )
                        1 -> DevicesScreen(state = state)
                        else -> SettingsScreen(state = state)
                    }
                }
            }
        }
    }
}

/** Bottom bar item: filled icon when selected, outlined otherwise. */
@Composable
private fun androidx.compose.foundation.layout.RowScope.TabItem(
    selected: Boolean,
    onClick: () -> Unit,
    filled: ImageVector,
    outlined: ImageVector,
    label: Int,
) {
    NavigationBarItem(
        selected = selected,
        onClick = onClick,
        icon = { Icon(if (selected) filled else outlined, contentDescription = null) },
        label = { Text(stringResource(label)) },
    )
}

/** Detail entry lookup used by the detail screen. */
fun AppState.findEntry(id: String): HistoryEntry? = engine.history.find(id)
