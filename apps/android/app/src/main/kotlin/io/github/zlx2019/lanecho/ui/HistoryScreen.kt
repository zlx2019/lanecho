package io.github.zlx2019.lanecho.ui

import android.graphics.BitmapFactory
import android.text.format.DateUtils
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.TextSnippet
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.outlined.ContentPaste
import androidx.compose.material.icons.outlined.Image
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import io.github.zlx2019.lanecho.R
import io.github.zlx2019.lanecho.core.history.EntryKind
import io.github.zlx2019.lanecho.core.history.HistoryEntry
import io.github.zlx2019.lanecho.state.AppState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HistoryScreen(
    state: AppState,
    modifier: Modifier = Modifier,
    onOpenDetail: (String) -> Unit,
    onGoPair: () -> Unit,
) {
    var query by remember { mutableStateOf("") }
    var searching by remember { mutableStateOf(false) }
    var confirmClear by remember { mutableStateOf(false) }
    var menuFor by remember { mutableStateOf<String?>(null) }

    // historyVersion/peersVersion drive re-reads from the engine
    val entries = remember(state.historyVersion, query) {
        if (query.isBlank()) state.engine.history.list() else state.engine.history.search(query)
    }
    val onlinePaired = remember(state.peersVersion) { state.engine.onlinePairedPeers().size }
    val hasPaired = remember(state.peersVersion) { state.engine.paired.all().isNotEmpty() }

    val topBar = TopAppBarDefaults.pinnedScrollBehavior()
    Column(
        modifier
            .fillMaxSize()
            .nestedScroll(topBar.nestedScrollConnection),
    ) {
        TopAppBar(
            scrollBehavior = topBar,
            title = {
                Column {
                    Text(stringResource(R.string.app_name))
                    Text(
                        text = when {
                            !state.online -> stringResource(R.string.status_offline)
                            onlinePaired > 0 -> stringResource(R.string.status_online, onlinePaired)
                            else -> stringResource(R.string.status_no_peers)
                        },
                        style = MaterialTheme.typography.labelMedium,
                        color = if (state.online && onlinePaired > 0) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            },
            actions = {
                IconButton(onClick = { searching = !searching; if (!searching) query = "" }) {
                    Icon(Icons.Filled.Search, contentDescription = stringResource(R.string.search_hint))
                }
                Box {
                    var overflow by remember { mutableStateOf(false) }
                    IconButton(onClick = { overflow = true }) { Text("⋮") }
                    DropdownMenu(expanded = overflow, onDismissRequest = { overflow = false }) {
                        DropdownMenuItem(
                            text = { Text(stringResource(R.string.menu_clear_history)) },
                            onClick = { overflow = false; confirmClear = true },
                        )
                    }
                }
            },
        )
        if (searching) {
            OutlinedTextField(
                value = query,
                onValueChange = { query = it },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 4.dp),
                placeholder = { Text(stringResource(R.string.search_hint)) },
                singleLine = true,
            )
        }

        if (entries.isEmpty()) {
            EmptyHistory(hasPaired = hasPaired, onGoPair = onGoPair)
        } else {
            LazyColumn(Modifier.fillMaxSize()) {
                items(entries, key = { it.id }) { entry ->
                    HistoryRow(
                        state = state,
                        entry = entry,
                        menuOpen = menuFor == entry.id,
                        onClick = { state.copyEntry(entry) },
                        onLongClick = { menuFor = entry.id },
                        onDismissMenu = { menuFor = null },
                        onOpenDetail = { onOpenDetail(entry.id) },
                        modifier = Modifier.animateItem(),
                    )
                }
            }
        }
    }

    if (confirmClear) {
        androidx.compose.material3.AlertDialog(
            onDismissRequest = { confirmClear = false },
            title = { Text(stringResource(R.string.clear_confirm_title)) },
            text = { Text(stringResource(R.string.clear_confirm_body)) },
            confirmButton = {
                androidx.compose.material3.TextButton(onClick = {
                    confirmClear = false
                    state.engine.history.clear()
                    state.onHistoryChanged()
                }) { Text(stringResource(R.string.dialog_confirm)) }
            },
            dismissButton = {
                androidx.compose.material3.TextButton(onClick = { confirmClear = false }) {
                    Text(stringResource(R.string.dialog_cancel))
                }
            },
        )
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun HistoryRow(
    state: AppState,
    entry: HistoryEntry,
    menuOpen: Boolean,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
    onDismissMenu: () -> Unit,
    onOpenDetail: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Box(modifier) {
        ListItem(
            modifier = Modifier.combinedClickable(onClick = onClick, onLongClick = onLongClick),
            leadingContent = { EntryThumbnail(state, entry) },
            headlineContent = {
                Text(entry.preview, maxLines = 1, overflow = TextOverflow.Ellipsis)
            },
            supportingContent = {
                val origin = entry.origin ?: stringResource(R.string.detail_source_local)
                val time = DateUtils.getRelativeTimeSpanString(entry.lastCopiedAt).toString()
                Text(
                    "$origin · $time",
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            },
            trailingContent = {
                if (entry.pinned) {
                    Icon(
                        Icons.Filled.PushPin, contentDescription = null,
                        modifier = Modifier.size(16.dp),
                        tint = MaterialTheme.colorScheme.primary,
                    )
                }
            },
        )
        DropdownMenu(expanded = menuOpen, onDismissRequest = onDismissMenu) {
            DropdownMenuItem(
                text = { Text(stringResource(R.string.menu_detail)) },
                onClick = { onDismissMenu(); onOpenDetail() },
            )
            DropdownMenuItem(
                text = {
                    Text(stringResource(if (entry.pinned) R.string.menu_unpin else R.string.menu_pin))
                },
                onClick = {
                    onDismissMenu()
                    state.engine.history.setPinned(entry.id, !entry.pinned)
                    state.onHistoryChanged()
                },
            )
            if (entry.kind == EntryKind.TEXT) {
                val shareContext = androidx.compose.ui.platform.LocalContext.current
                DropdownMenuItem(
                    text = { Text(stringResource(R.string.menu_share)) },
                    onClick = { onDismissMenu(); shareEntry(shareContext, entry) },
                )
            }
            DropdownMenuItem(
                text = { Text(stringResource(R.string.menu_delete)) },
                onClick = {
                    onDismissMenu()
                    state.engine.history.delete(entry.id)
                    state.onHistoryChanged()
                },
            )
        }
    }
}

/** Rounded leading tile: image thumbnail, or a kind icon on a tinted plate. */
@Composable
private fun EntryThumbnail(state: AppState, entry: HistoryEntry) {
    val shape = RoundedCornerShape(10.dp)
    Box(
        Modifier
            .size(44.dp)
            .clip(shape)
            .background(MaterialTheme.colorScheme.surfaceVariant),
        contentAlignment = Alignment.Center,
    ) {
        val thumbnail = if (entry.kind == EntryKind.IMAGE) rememberBlobThumbnail(state, entry.blobHash) else null
        when {
            thumbnail != null -> Image(
                thumbnail,
                contentDescription = null,
                modifier = Modifier.fillMaxSize(),
                contentScale = ContentScale.Crop,
            )
            entry.kind == EntryKind.IMAGE -> Icon(
                Icons.Outlined.Image, contentDescription = null,
                modifier = Modifier.size(22.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            else -> Icon(
                Icons.AutoMirrored.Outlined.TextSnippet, contentDescription = null,
                modifier = Modifier.size(22.dp),
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun EmptyHistory(hasPaired: Boolean, onGoPair: () -> Unit) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.padding(32.dp)) {
            Icon(
                Icons.Outlined.ContentPaste,
                contentDescription = null,
                modifier = Modifier
                    .size(88.dp)
                    .clip(RoundedCornerShape(24.dp))
                    .background(MaterialTheme.colorScheme.primaryContainer.copy(alpha = 0.5f))
                    .padding(24.dp),
                tint = MaterialTheme.colorScheme.primary,
            )
            Text(
                stringResource(R.string.empty_history_title),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.padding(top = 20.dp),
            )
            Text(
                stringResource(R.string.empty_history_body),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )
            if (!hasPaired) {
                OutlinedButton(onClick = onGoPair, modifier = Modifier.padding(top = 16.dp)) {
                    Text(stringResource(R.string.empty_go_pair))
                }
            }
        }
    }
}

/** Decode a small thumbnail for the list row off the main thread. */
@Composable
fun rememberBlobThumbnail(state: AppState, blobHash: String?): ImageBitmap? {
    var bitmap by remember(blobHash) { mutableStateOf<ImageBitmap?>(null) }
    LaunchedEffect(blobHash) {
        val hash = blobHash ?: return@LaunchedEffect
        bitmap = withContext(Dispatchers.IO) {
            runCatching {
                val path = state.engine.history.blobPath(hash).toFile()
                val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
                BitmapFactory.decodeFile(path.absolutePath, bounds)
                val sample = maxOf(1, minOf(bounds.outWidth, bounds.outHeight) / 96)
                BitmapFactory.decodeFile(
                    path.absolutePath,
                    BitmapFactory.Options().apply { inSampleSize = sample },
                )?.asImageBitmap()
            }.getOrNull()
        }
    }
    return bitmap
}

/** Forward a text entry into the system share sheet. */
internal fun shareEntry(context: android.content.Context, entry: HistoryEntry) {
    val text = entry.text ?: return
    val intent = android.content.Intent(android.content.Intent.ACTION_SEND).apply {
        type = "text/plain"
        putExtra(android.content.Intent.EXTRA_TEXT, text)
    }
    context.startActivity(android.content.Intent.createChooser(intent, null))
}
