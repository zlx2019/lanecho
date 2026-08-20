package io.github.zlx2019.lanecho.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
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
import io.github.zlx2019.lanecho.core.sync.Settings
import io.github.zlx2019.lanecho.state.AppState
import io.github.zlx2019.lanecho.sync.SyncService

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(state: AppState, modifier: Modifier = Modifier) {
    // settingsVersion: settings changes are rare, drive updates locally
    var version by remember { mutableStateOf(0) }
    val settings = remember(version) { state.engine.settings.current }
    fun save(update: Settings) {
        state.engine.settings.save(update)
        state.engine.history.maxEntries = update.historyLimit
        version++
    }

    var editName by remember { mutableStateOf(false) }
    var editPort by remember { mutableStateOf(false) }
    var editLimit by remember { mutableStateOf(false) }

    Column(
        modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState()),
    ) {
        TopAppBar(title = { Text(stringResource(R.string.tab_settings)) })

        Section(stringResource(R.string.settings_general))
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_device_name)) },
            supportingContent = { Text(state.engine.localInfo().name) },
            modifier = Modifier.clickable { editName = true },
        )
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_fingerprint)) },
            supportingContent = {
                val fp = state.engine.identity.fingerprint
                Text(fp.take(16) + "…" + fp.takeLast(8), style = MaterialTheme.typography.bodySmall)
            },
        )

        Section(stringResource(R.string.settings_sync))
        SwitchRow(stringResource(R.string.settings_receive_text), settings.receiveText) {
            save(settings.copy(receiveText = it))
        }
        SwitchRow(stringResource(R.string.settings_receive_images), settings.receiveImages) {
            save(settings.copy(receiveImages = it))
        }
        SwitchRow(
            stringResource(R.string.settings_auto_write), settings.autoWriteClipboard,
            hint = stringResource(R.string.settings_auto_write_hint),
        ) { save(settings.copy(autoWriteClipboard = it)) }
        SwitchRow(stringResource(R.string.settings_send_on_open), settings.sendOnOpen) {
            save(settings.copy(sendOnOpen = it))
        }
        SwitchRow(
            stringResource(R.string.settings_background_online), settings.backgroundOnline,
            hint = stringResource(R.string.settings_background_online_hint),
        ) { enabled ->
            save(settings.copy(backgroundOnline = enabled))
            if (enabled) SyncService.start(state.appContext) else SyncService.stop(state.appContext)
        }
        BatteryOptimizationRow(state)
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_port)) },
            supportingContent = { Text(settings.port.toString() + " · " + stringResource(R.string.settings_port_hint)) },
            modifier = Modifier.clickable { editPort = true },
        )

        Section(stringResource(R.string.settings_storage))
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_history_limit)) },
            supportingContent = { Text(settings.historyLimit.toString()) },
            modifier = Modifier.clickable { editLimit = true },
        )
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_usage)) },
            supportingContent = {
                val usage = remember(state.historyVersion) { state.engine.history.diskUsage() }
                Text(formatBytes(usage))
            },
        )

        Section(stringResource(R.string.settings_about))
        ListItem(
            headlineContent = { Text(stringResource(R.string.app_name)) },
            supportingContent = {
                val versionName = remember {
                    runCatching {
                        state.appContext.packageManager
                            .getPackageInfo(state.appContext.packageName, 0).versionName
                    }.getOrNull() ?: "?"
                }
                Text(stringResource(R.string.settings_version, versionName))
            },
        )
    }

    if (editName) {
        TextDialog(
            title = stringResource(R.string.settings_device_name),
            initial = state.engine.identity.displayName ?: "",
            onDismiss = { editName = false },
        ) { value ->
            editName = false
            state.engine.rename(value.ifBlank { null })
            version++
        }
    }
    if (editPort) {
        TextDialog(
            title = stringResource(R.string.settings_port),
            initial = settings.port.toString(),
            onDismiss = { editPort = false },
        ) { value ->
            editPort = false
            value.toIntOrNull()?.takeIf { it in 1024..65535 }?.let { save(settings.copy(port = it)) }
        }
    }
    if (editLimit) {
        TextDialog(
            title = stringResource(R.string.settings_history_limit),
            initial = settings.historyLimit.toString(),
            onDismiss = { editLimit = false },
        ) { value ->
            editLimit = false
            value.toIntOrNull()?.takeIf { it in 10..10_000 }?.let { save(settings.copy(historyLimit = it)) }
        }
    }
}

/**
 * Doze suspends background networking even for foreground services; the
 * exemption keeps receiving alive with the screen off. State refreshes on
 * every recomposition of the settings tab.
 */
@Composable
private fun BatteryOptimizationRow(state: AppState) {
    val context = state.appContext
    var refresh by remember { mutableStateOf(0) }
    val exempted = remember(refresh) {
        val power = context.getSystemService(android.os.PowerManager::class.java)
        power?.isIgnoringBatteryOptimizations(context.packageName) == true
    }
    ListItem(
        headlineContent = { Text(stringResource(R.string.settings_battery_opt)) },
        supportingContent = {
            Text(
                stringResource(
                    if (exempted) R.string.settings_battery_opt_done else R.string.settings_battery_opt_hint,
                ),
            )
        },
        modifier = Modifier.clickable(enabled = !exempted) {
            runCatching {
                val intent = android.content.Intent(
                    android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                    android.net.Uri.parse("package:" + context.packageName),
                ).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
                context.startActivity(intent)
            }
            refresh++
        },
    )
}

@Composable
private fun Section(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, top = 20.dp, bottom = 4.dp),
    )
}

@Composable
private fun SwitchRow(title: String, checked: Boolean, hint: String? = null, onChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = hint?.let { { Text(it, style = MaterialTheme.typography.bodySmall) } },
        trailingContent = { Switch(checked = checked, onCheckedChange = onChange) },
    )
}

@Composable
private fun TextDialog(title: String, initial: String, onDismiss: () -> Unit, onConfirm: (String) -> Unit) {
    var value by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            OutlinedTextField(value = value, onValueChange = { value = it }, singleLine = true, modifier = Modifier.fillMaxWidth())
        },
        confirmButton = {
            TextButton(onClick = { onConfirm(value) }) { Text(stringResource(R.string.settings_save)) }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.dialog_cancel)) }
        },
    )
}

private fun formatBytes(bytes: Long): String = when {
    bytes >= 1024 * 1024 -> "%.1f MB".format(bytes / 1048576.0)
    bytes >= 1024 -> "%.1f KB".format(bytes / 1024.0)
    else -> "$bytes B"
}
