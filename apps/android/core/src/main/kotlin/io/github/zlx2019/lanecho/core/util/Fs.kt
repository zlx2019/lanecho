package io.github.zlx2019.lanecho.core.util

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.StandardCopyOption

// Write-then-rename so a crash mid-write can never leave a truncated file
// (the same discipline as the Rust/Swift stores)
fun atomicWrite(path: Path, bytes: ByteArray) {
    path.parent?.let { Files.createDirectories(it) }
    val tmp = path.resolveSibling(path.fileName.toString() + ".tmp")
    Files.write(tmp, bytes)
    try {
        Files.move(tmp, path, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
    } catch (_: java.nio.file.AtomicMoveNotSupportedException) {
        Files.move(tmp, path, StandardCopyOption.REPLACE_EXISTING)
    }
}
