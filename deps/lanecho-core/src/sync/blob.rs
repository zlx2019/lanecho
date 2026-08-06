//! IO and landing helpers for blob sync: sending and receiving the raw byte
//! streams behind images and files.
//!
//! Stream shape: the offer (a JSON frame) is followed by **continuous raw
//! bytes** — 1MiB buffered reads and writes, no per-chunk frame header —
//! terminated by a BlobFooter frame carrying the BLAKE3 of the whole stream.
//! Integrity is computed as the bytes go by; the sending side never pre-reads
//! a file just to hash it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::{IDLE_TIMEOUT, SYNC_STREAM_BUF};
use crate::protocol::FileMeta;

use super::SyncError;

/// Name of the landing directory for received files, under the data
/// directory. History entry deletion and the startup sweep recognise it by
/// this prefix: **the user's own files are never inside it, and the prefix is
/// what keeps cleanup off them**.
pub const SYNC_FILES_DIR: &str = "sync-files";

/// Stream out a block of in-memory bytes (an image PNG); returns the BLAKE3
/// hex of the whole stream
pub(super) async fn send_bytes<S>(stream: &mut S, bytes: &[u8]) -> Result<String, SyncError>
where
    S: AsyncWrite + Unpin,
{
    let mut hasher = blake3::Hasher::new();
    for chunk in bytes.chunks(SYNC_STREAM_BUF) {
        stream.write_all(chunk).await?;
        hasher.update(chunk);
    }
    stream.flush().await?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Stream out a set of files, concatenated in meta order; returns the BLAKE3
/// hex of the whole stream
///
/// Both ways a file can be changed underneath us mid-send have to abort rather
/// than improvise:
/// - It shrank (EOF before the declared byte count): sending on would leave
///   the total stream length wrong and the peer's read would hang until it
///   timed out, so error out and drop the connection; the peer cleans up as
///   for an unfinished transfer;
/// - It grew: send only the declared byte count, since the extra is not part
///   of this snapshot.
pub(super) async fn send_files<S>(
    stream: &mut S,
    paths: &[PathBuf],
    metas: &[FileMeta],
) -> Result<String, SyncError>
where
    S: AsyncWrite + Unpin,
{
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; SYNC_STREAM_BUF];
    for (path, meta) in paths.iter().zip(metas) {
        let mut file = tokio::fs::File::open(path).await?;
        let mut remaining = meta.bytes;
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = file.read(&mut buf[..want]).await?;
            if n == 0 {
                return Err(SyncError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    format!("文件在发送途中变小: {}", path.display()),
                )));
            }
            stream.write_all(&buf[..n]).await?;
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
    }
    stream.flush().await?;
    Ok(hasher.finalize().to_hex().to_string())
}

/// Receive a raw byte stream into memory (images; the caller has already
/// checked total against the cap from the offer). Returns the bytes and the
/// BLAKE3 hex of the whole stream. Each read carries the same timeout as the
/// gap between frames, so a stream cut halfway does not hang
pub(super) async fn recv_bytes<S>(
    stream: &mut S,
    total: u64,
) -> Result<(Vec<u8>, String), SyncError>
where
    S: AsyncRead + Unpin,
{
    let mut hasher = blake3::Hasher::new();
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = vec![0u8; SYNC_STREAM_BUF];
    let mut remaining = total;
    while remaining > 0 {
        let want = remaining.min(buf.len() as u64) as usize;
        let n = tokio::time::timeout(IDLE_TIMEOUT, stream.read(&mut buf[..want]))
            .await
            .map_err(|_| SyncError::Timeout("blob_stream"))??;
        if n == 0 {
            return Err(SyncError::Io(std::io::ErrorKind::UnexpectedEof.into()));
        }
        out.extend_from_slice(&buf[..n]);
        hasher.update(&buf[..n]);
        remaining -= n as u64;
    }
    Ok((out, hasher.finalize().to_hex().to_string()))
}

/// Receive a raw byte stream, split it by the manifest, and stream each piece
/// into a `.part` temporary file
///
/// Returns (the temporary file paths, the BLAKE3 hex of the whole stream).
/// Nothing is renamed before the checksum passes — the caller handles the
/// aftermath: [`finalize_parts`] on success, delete the whole batch directory
/// on failure.
pub(super) async fn recv_files<S>(
    stream: &mut S,
    metas: &[FileMeta],
    batch_dir: &Path,
) -> Result<(Vec<PathBuf>, String), SyncError>
where
    S: AsyncRead + Unpin,
{
    tokio::fs::create_dir_all(batch_dir).await?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; SYNC_STREAM_BUF];
    let mut parts = Vec::with_capacity(metas.len());
    for (index, meta) in metas.iter().enumerate() {
        let part = batch_dir.join(format!("recv-{index}.part"));
        let mut file = tokio::fs::File::create(&part).await?;
        let mut remaining = meta.bytes;
        while remaining > 0 {
            let want = remaining.min(buf.len() as u64) as usize;
            let n = tokio::time::timeout(IDLE_TIMEOUT, stream.read(&mut buf[..want]))
                .await
                .map_err(|_| SyncError::Timeout("blob_stream"))??;
            if n == 0 {
                return Err(SyncError::Io(std::io::ErrorKind::UnexpectedEof.into()));
            }
            file.write_all(&buf[..n]).await?;
            hasher.update(&buf[..n]);
            remaining -= n as u64;
        }
        file.flush().await?;
        parts.push(part);
    }
    Ok((parts, hasher.finalize().to_hex().to_string()))
}

/// Name the files once the checksum passes: `.part` → the sanitized final name,
/// with a counter appended for duplicates within the batch
///
/// Returns the final paths in manifest order, for writing to the clipboard and
/// recording in history.
pub(super) async fn finalize_parts(
    parts: &[PathBuf],
    metas: &[FileMeta],
    batch_dir: &Path,
) -> Result<Vec<PathBuf>, SyncError> {
    let mut used = HashSet::new();
    let mut finals = Vec::with_capacity(parts.len());
    for (index, (part, meta)) in parts.iter().zip(metas).enumerate() {
        let name = unique_name(&mut used, sanitize_file_name(&meta.name, index));
        let target = batch_dir.join(&name);
        tokio::fs::rename(part, &target).await?;
        finals.push(target);
    }
    Ok(finals)
}

/// Sanitize a file name; security-critical, since the name comes from the peer
/// and cannot be trusted
///
/// A reduced rule set suffices because no directory tree is transferred, only
/// flat file names: take the basename first, which strips every path component
/// (a traversal payload like `../../x` is down to `x` by this point), then
/// clean platform-illegal characters and Windows reserved names.
/// **It never rejects**: an empty or all-dots result falls back to
/// `file-<i>` — one malicious name should not fail the whole batch.
pub(super) fn sanitize_file_name(raw: &str, fallback_index: usize) -> String {
    let base = Path::new(raw)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '/' | '\\') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_end_matches([' ', '.']).to_string();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        return format!("file-{fallback_index}");
    }
    // Windows reserved names (CON/PRN/…) are sanitized even on macOS: the data
    // directory can be carried onto a Windows machine by a network or sync
    // drive
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        return format!("_{cleaned}");
    }
    cleaned
}

/// Resolve duplicate names within a batch: `a.txt` → `a (1).txt`, counting up
pub(super) fn unique_name(used: &mut HashSet<String>, name: String) -> String {
    if used.insert(name.clone()) {
        return name;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        // A hidden file like ".bashrc" is taken as one whole stem rather than
        // split into an empty prefix
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    for i in 1.. {
        let candidate = format!("{stem} ({i}){ext}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("计数器耗尽前必然找到空位")
}

/// Decode PNG to RGBA8 when landing a received image; blocking CPU work, so
/// the caller puts it on spawn_blocking
///
/// The png crate's expansion transforms bring palette, grayscale and RGB all
/// to RGBA8 — interoperating across implementations, the sub-format the peer
/// (Swift/ImageIO) encoded is not ours to control.
pub(super) fn decode_png_rgba(
    bytes: &[u8],
) -> Result<(usize, usize, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info()?;
    let mut buf = vec![0u8; reader.output_buffer_size().ok_or("PNG 尺寸溢出")?];
    let info = reader.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let (width, height) = (info.width as usize, info.height as usize);
    let rgba = match (info.color_type, info.bit_depth) {
        (png::ColorType::Rgba, png::BitDepth::Eight) => buf,
        (png::ColorType::Rgb, png::BitDepth::Eight) => {
            let mut out = Vec::with_capacity(width * height * 4);
            for px in buf.chunks_exact(3) {
                out.extend_from_slice(px);
                out.push(0xFF);
            }
            out
        }
        (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
            let mut out = Vec::with_capacity(width * height * 4);
            for px in buf.chunks_exact(2) {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }
            out
        }
        (png::ColorType::Grayscale, png::BitDepth::Eight) => {
            let mut out = Vec::with_capacity(width * height * 4);
            for px in &buf {
                out.extend_from_slice(&[*px, *px, *px, 0xFF]);
            }
            out
        }
        other => return Err(format!("不支持的 PNG 子格式: {other:?}").into()),
    };
    Ok((width, height, rgba))
}

/// Encode RGBA8 as PNG on the sending side; blocking CPU work, so the caller
/// puts it on spawn_blocking
///
/// The compression level must be Fast: at 2560×1440, Balanced measures 545ms
/// against Fast's 24ms — 22 to 28 times the cost — for only ±5% in size. Do
/// not change it back.
pub(super) fn encode_png_rgba(
    width: usize,
    height: usize,
    rgba: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every path traversal payload reduces to a safe basename
    #[test]
    fn sanitize_blocks_traversal() {
        assert_eq!(sanitize_file_name("../../etc/passwd", 0), "passwd");
        assert_eq!(sanitize_file_name("/etc/shadow", 0), "shadow");
        // A backslash is not a path separator on Unix, so file_name does not
        // split on it — but once replaced with _ the whole string is just a
        // harmless ordinary file name: no separator, and not ".."
        assert_eq!(sanitize_file_name("..\\..\\win.ini", 0), ".._.._win.ini");
        assert_eq!(sanitize_file_name("..", 0), "file-0");
        assert_eq!(sanitize_file_name("", 3), "file-3");
        assert_eq!(sanitize_file_name("...", 5), "file-5");
    }

    /// Illegal Windows characters replaced, trailing dots and spaces stripped,
    /// reserved names prefixed
    #[test]
    fn sanitize_windows_rules() {
        assert_eq!(sanitize_file_name("a<b>c:d.txt", 0), "a_b_c_d.txt");
        assert_eq!(sanitize_file_name("name. ", 0), "name");
        assert_eq!(sanitize_file_name("CON.txt", 0), "_CON.txt");
        assert_eq!(sanitize_file_name("COM1", 0), "_COM1");
        assert_eq!(sanitize_file_name("COMMON.txt", 0), "COMMON.txt");
        assert_eq!(
            sanitize_file_name("正常文件 名.tar.gz", 0),
            "正常文件 名.tar.gz"
        );
    }

    /// Duplicates within a batch count up; a hidden file is not split into an
    /// empty stem
    #[test]
    fn unique_names_increment() {
        let mut used = HashSet::new();
        assert_eq!(unique_name(&mut used, "a.txt".into()), "a.txt");
        assert_eq!(unique_name(&mut used, "a.txt".into()), "a (1).txt");
        assert_eq!(unique_name(&mut used, "a.txt".into()), "a (2).txt");
        assert_eq!(unique_name(&mut used, ".bashrc".into()), ".bashrc");
        assert_eq!(unique_name(&mut used, ".bashrc".into()), ".bashrc (1)");
    }

    /// PNG encode/decode round trip: RGBA comes back byte-for-byte, which is
    /// what the echo hash baseline rests on
    #[test]
    fn png_roundtrip_preserves_rgba() {
        let (w, h) = (3, 2);
        let rgba: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let png = encode_png_rgba(w, h, &rgba).unwrap();
        let (dw, dh, decoded) = decode_png_rgba(&png).unwrap();
        assert_eq!((dw, dh), (w, h));
        assert_eq!(decoded, rgba);
    }

    /// Decoding an RGB PNG (no alpha) fills in an opaque alpha: sub-format
    /// compatibility across implementations
    #[test]
    fn png_decode_expands_rgb() {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[10, 20, 30, 40, 50, 60]).unwrap();
        }
        let (w, h, rgba) = decode_png_rgba(&out).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba, vec![10, 20, 30, 255, 40, 50, 60, 255]);
    }
}
