package io.github.zlx2019.lanecho.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Checkbox
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExposedDropdownMenuBox
import androidx.compose.material3.ExposedDropdownMenuDefaults
import androidx.compose.material3.ListItem
import androidx.compose.material3.MenuAnchorType
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import io.github.zlx2019.lanecho.R
import io.github.zlx2019.lanecho.capture.ClipboardCapture
import io.github.zlx2019.lanecho.capture.ShizukuClipboard
import io.github.zlx2019.lanecho.core.sync.BackgroundReadMethod
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
        // Checkboxes: one row for both receive kinds
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_receive_kinds)) },
            trailingContent = {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    LabeledCheckbox(
                        label = stringResource(R.string.settings_receive_text_short),
                        checked = settings.receiveText,
                    ) { save(settings.copy(receiveText = it)) }
                    Spacer(Modifier.width(16.dp))
                    LabeledCheckbox(
                        label = stringResource(R.string.settings_receive_images_short),
                        checked = settings.receiveImages,
                    ) { save(settings.copy(receiveImages = it)) }
                }
            },
        )
        SwitchRow(stringResource(R.string.settings_auto_write), settings.autoWriteClipboard) {
            save(settings.copy(autoWriteClipboard = it))
        }
        SwitchRow(stringResource(R.string.settings_background_online), settings.backgroundOnline) { enabled ->
            save(settings.copy(backgroundOnline = enabled))
            if (enabled) SyncService.start(state.appContext) else SyncService.stop(state.appContext)
        }
        BatteryOptimizationRow(state)
        BackgroundReadRow(state, settings) { save(it) }
        ListItem(
            headlineContent = { Text(stringResource(R.string.settings_port)) },
            supportingContent = { Text(settings.port.toString()) },
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
 * Background capture method picker plus whatever that method still needs
 * (K6). Android blocks background clipboard reads outright, so each method
 * trades setup effort against how invisible it is; the row below the picker
 * always states the current blocker, never fails silently.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun BackgroundReadRow(state: AppState, settings: Settings, save: (Settings) -> Unit) {
    val context = LocalContext.current
    var expanded by remember { mutableStateOf(false) }
    var probe by remember { mutableStateOf(0) }
    val method = settings.backgroundReadMethod
    val readiness = remember(method, probe) { state.capture.readiness() }

    val labels = mapOf(
        BackgroundReadMethod.OFF to stringResource(R.string.settings_capture_off),
        BackgroundReadMethod.POLLING to stringResource(R.string.settings_capture_polling),
        BackgroundReadMethod.LOGS to stringResource(R.string.settings_capture_logs),
        BackgroundReadMethod.SHIZUKU to stringResource(R.string.settings_capture_shizuku),
    )
    Column(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) {
        Text(
            stringResource(R.string.settings_capture),
            style = MaterialTheme.typography.bodyLarge,
            modifier = Modifier.padding(bottom = 8.dp),
        )
        ExposedDropdownMenuBox(expanded = expanded, onExpandedChange = { expanded = it }) {
            OutlinedTextField(
                value = labels[method] ?: labels.getValue(BackgroundReadMethod.OFF),
                onValueChange = {},
                readOnly = true,
                singleLine = true,
                trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                colors = ExposedDropdownMenuDefaults.outlinedTextFieldColors(),
                modifier = Modifier
                    .fillMaxWidth()
                    .menuAnchor(MenuAnchorType.PrimaryNotEditable),
            )
            ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                labels.forEach { (value, label) ->
                    DropdownMenuItem(
                        text = { Text(label) },
                        onClick = {
                            expanded = false
                            save(settings.copy(backgroundReadMethod = value))
                            state.capture.refresh()
                            probe++
                        },
                    )
                }
            }
        }
    }
    if (method != BackgroundReadMethod.OFF) {
        CaptureReadinessRow(state, readiness, context) { probe++ }
    }
}

/** One actionable line describing what the chosen method is still missing. */
@Composable
private fun CaptureReadinessRow(
    state: AppState,
    readiness: ClipboardCapture.Readiness,
    context: android.content.Context,
    onChanged: () -> Unit,
) {
    val packageName = context.packageName
    val (text, action) = when (readiness) {
        ClipboardCapture.Readiness.READY ->
            stringResource(R.string.settings_capture_ready) to null
        ClipboardCapture.Readiness.NEEDS_OVERLAY ->
            stringResource(R.string.settings_capture_needs_overlay) to {
                runCatching {
                    context.startActivity(
                        android.content.Intent(
                            android.provider.Settings.ACTION_MANAGE_OVERLAY_PERMISSION,
                            android.net.Uri.parse("package:$packageName"),
                        ).addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK),
                    )
                }
                onChanged()
            }
        ClipboardCapture.Readiness.NEEDS_READ_LOGS -> {
            // The command is long and must be typed on a computer: copying it
            // beats asking the user to transcribe it
            val command = "adb shell pm grant $packageName android.permission.READ_LOGS"
            stringResource(R.string.settings_capture_needs_logs) to {
                io.github.zlx2019.lanecho.platform.ClipboardPort.writeText(context, command)
                state.showToast(context.getString(R.string.settings_capture_command_copied))
                Unit
            }
        }
        ClipboardCapture.Readiness.NEEDS_SHIZUKU_SERVICE ->
            stringResource(R.string.settings_capture_needs_shizuku) to null
        ClipboardCapture.Readiness.NEEDS_SHIZUKU_PERMISSION ->
            stringResource(R.string.settings_capture_needs_shizuku_grant) to {
                ShizukuClipboard.requestPermission()
                onChanged()
            }
    }
    ListItem(
        headlineContent = {
            Text(
                text,
                style = MaterialTheme.typography.bodySmall,
                color = if (readiness == ClipboardCapture.Readiness.READY) {
                    MaterialTheme.colorScheme.onSurfaceVariant
                } else {
                    MaterialTheme.colorScheme.error
                },
            )
        },
        modifier = if (action != null) Modifier.clickable { action() } else Modifier,
    )
}

/** Compact checkbox with a tappable label (no independent touch target). */
@Composable
private fun LabeledCheckbox(label: String, checked: Boolean, onChange: (Boolean) -> Unit) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.clickable { onChange(!checked) },
    ) {
        Checkbox(checked = checked, onCheckedChange = null)
        Text(label, style = MaterialTheme.typography.bodyMedium)
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
