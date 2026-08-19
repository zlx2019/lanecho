package io.github.zlx2019.lanecho.ui

import android.graphics.BitmapFactory
import android.text.format.DateFormat
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import io.github.zlx2019.lanecho.R
import io.github.zlx2019.lanecho.findEntry
import io.github.zlx2019.lanecho.core.history.EntryKind
import io.github.zlx2019.lanecho.state.AppState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** Render cap shared with the desktop detail card (20k chars). */
private const val DETAIL_TEXT_LIMIT = 20_000

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DetailScreen(state: AppState, entryId: String, onClose: () -> Unit) {
    val entry = remember(entryId, state.historyVersion) { state.findEntry(entryId) }
    if (entry == null) {
        LaunchedEffect(Unit) { onClose() }
        return
    }
    val context = LocalContext.current

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.detail_title)) },
                navigationIcon = {
                    IconButton(onClick = onClose) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = null)
                    }
                },
            )
        },
    ) { padding ->
        Column(
            Modifier
                .padding(padding)
                .fillMaxSize()
                .padding(16.dp),
        ) {
            Card(
                Modifier
                    .fillMaxWidth()
                    .weight(1f, fill = false),
            ) {
                if (entry.kind == EntryKind.IMAGE) {
                    var image by remember(entry.blobHash) { mutableStateOf<ImageBitmap?>(null) }
                    LaunchedEffect(entry.blobHash) {
                        val hash = entry.blobHash ?: return@LaunchedEffect
                        image = withContext(Dispatchers.IO) {
                            runCatching {
                                BitmapFactory.decodeFile(state.engine.history.blobPath(hash).toFile().absolutePath)
                                    ?.asImageBitmap()
                            }.getOrNull()
                        }
                    }
                    image?.let {
                        Image(
                            it, contentDescription = null,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(12.dp),
                        )
                    }
                } else {
                    val text = entry.text.orEmpty()
                    Text(
                        text = if (text.length > DETAIL_TEXT_LIMIT) text.take(DETAIL_TEXT_LIMIT) else text,
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier
                            .fillMaxWidth()
                            .verticalScroll(rememberScrollState())
                            .padding(12.dp),
                    )
                }
            }

            HorizontalDivider(Modifier.padding(vertical = 12.dp))

            MetaRow(
                stringResource(R.string.detail_type),
                if (entry.kind == EntryKind.IMAGE) {
                    stringResource(R.string.detail_type_image) + " · " + entry.preview
                } else {
                    stringResource(R.string.detail_type_text, byteSize(entry.text.orEmpty()))
                },
            )
            MetaRow(
                stringResource(R.string.detail_source),
                entry.origin ?: stringResource(R.string.detail_source_local),
            )
            MetaRow(
                stringResource(R.string.detail_time),
                DateFormat.format("yyyy-MM-dd HH:mm:ss", entry.lastCopiedAt).toString(),
            )
            MetaRow(stringResource(R.string.detail_count), entry.copyCount.toString())

            Row(Modifier.padding(top = 16.dp)) {
                Button(onClick = { state.copyEntry(entry) }, modifier = Modifier.weight(1f)) {
                    Text(stringResource(R.string.action_copy))
                }
                if (entry.kind == EntryKind.TEXT) {
                    OutlinedButton(
                        onClick = { shareEntry(context, entry) },
                        modifier = Modifier
                            .weight(1f)
                            .padding(start = 8.dp),
                    ) { Text(stringResource(R.string.menu_share)) }
                }
                OutlinedButton(
                    onClick = {
                        state.engine.history.delete(entry.id)
                        state.onHistoryChanged()
                        onClose()
                    },
                    modifier = Modifier
                        .weight(1f)
                        .padding(start = 8.dp),
                ) { Text(stringResource(R.string.menu_delete)) }
            }
        }
    }
}

@Composable
private fun MetaRow(label: String, value: String) {
    Row(Modifier.padding(vertical = 2.dp)) {
        Text(
            label,
            style = MaterialTheme.typography.labelLarge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.weight(0.3f),
        )
        Text(value, style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(0.7f))
    }
}

private fun byteSize(text: String): String {
    val bytes = text.encodeToByteArray().size
    return when {
        bytes >= 1024 * 1024 -> "%.1f MB".format(bytes / 1048576.0)
        bytes >= 1024 -> "%.1f KB".format(bytes / 1024.0)
        else -> "$bytes B"
    }
}
