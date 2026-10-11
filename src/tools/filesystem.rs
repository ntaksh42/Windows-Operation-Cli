//! `FileSystem` tool: read/write/copy/move/delete/list/search/info operations.
//!
//! Relative paths resolve against the user's Desktop folder. All errors are
//! returned as formatted text (never as an MCP `isError`), matching the
//! Python reference implementation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use rmcp::schemars;
use serde::Deserialize;

use crate::params::{BoolOrString, opt_bool};

/// Maximum file size accepted by `read` (10 MB).
const MAX_READ_SIZE: u64 = 10 * 1024 * 1024;
/// Maximum number of entries returned by `list`/`search` before truncation.
const MAX_RESULTS: usize = 500;
const MAX_SCAN_ENTRIES: usize = 100_000;

/// Parameters for the `FileSystem` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FileSystemParams {
    /// Operation to perform.
    #[schemars(
        description = "Operation: read, write, edit, copy, move, delete, list, search, info."
    )]
    pub mode: FileSystemMode,
    /// Target path. Relative paths resolve against the Desktop folder.
    #[schemars(description = "Target path. Relative paths resolve against the Desktop folder.")]
    pub path: String,
    /// Destination path for copy/move. Relative paths resolve against the Desktop folder.
    #[schemars(description = "Destination path for copy/move mode.")]
    pub destination: Option<String>,
    /// Content to write for write mode.
    #[schemars(description = "Content to write (required for write mode).")]
    pub content: Option<String>,
    /// Exact text to replace in edit mode.
    pub old_text: Option<String>,
    /// Replacement text for edit mode.
    pub new_text: Option<String>,
    /// Glob pattern for search (required) or list (optional) mode.
    #[schemars(description = "Glob pattern for search (required) or list (optional) mode.")]
    pub pattern: Option<String>,
    /// Regular expression searched within glob-matching files in search mode.
    pub content_pattern: Option<String>,
    /// Recurse into subdirectories (list/search) or delete non-empty directories.
    #[serde(default)]
    #[schemars(
        description = "Recurse into subdirectories, or allow deleting non-empty directories."
    )]
    pub recursive: Option<BoolOrString>,
    /// Append to the file instead of overwriting it (write mode).
    #[serde(default)]
    #[schemars(description = "Append instead of overwrite (write mode).")]
    pub append: Option<BoolOrString>,
    /// Allow overwriting an existing destination (copy/move mode).
    #[serde(default)]
    #[schemars(description = "Allow overwriting an existing destination (copy/move mode).")]
    pub overwrite: Option<BoolOrString>,
    /// 1-based starting line for read mode.
    pub offset: Option<i64>,
    /// Maximum number of lines to read for read mode.
    pub limit: Option<i64>,
    /// Text encoding (WHATWG label such as utf-8, utf-16le, shift_jis, or windows-1252).
    #[serde(default = "default_encoding")]
    #[schemars(description = "Text encoding label (default utf-8).")]
    pub encoding: String,
    /// Include dotfile entries in list mode.
    #[serde(default)]
    #[schemars(description = "Include dotfile entries in list mode.")]
    pub show_hidden: Option<BoolOrString>,
    /// Preview delete targets without removing them.
    #[serde(default)]
    pub dry_run: Option<BoolOrString>,
}

fn default_encoding() -> String {
    "utf-8".to_string()
}

/// `FileSystem` operation mode.
#[derive(Debug, Deserialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum FileSystemMode {
    Read,
    Write,
    Edit,
    Copy,
    Move,
    Delete,
    List,
    Search,
    Info,
}

/// Runs the `FileSystem` tool and returns a caller-facing text response.
///
/// Expected errors (missing files, permission issues, bad parameters) are
/// returned as `"Error: ..."` text rather than propagated, matching the
/// Python reference implementation.
pub fn file_system(params: FileSystemParams) -> String {
    let recursive = match opt_bool(&params.recursive, false) {
        Ok(v) => v,
        Err(e) => return format!("Error: {e}"),
    };
    let append = match opt_bool(&params.append, false) {
        Ok(v) => v,
        Err(e) => return format!("Error: {e}"),
    };
    let overwrite = match opt_bool(&params.overwrite, false) {
        Ok(v) => v,
        Err(e) => return format!("Error: {e}"),
    };
    let show_hidden = match opt_bool(&params.show_hidden, false) {
        Ok(v) => v,
        Err(e) => return format!("Error: {e}"),
    };
    let dry_run = match opt_bool(&params.dry_run, false) {
        Ok(v) => v,
        Err(e) => return format!("Error: {e}"),
    };

    let Some(encoding) = encoding_rs::Encoding::for_label(params.encoding.as_bytes()) else {
        return format!("Error: Unsupported encoding \"{}\".", params.encoding);
    };

    let base = desktop_dir();
    let path = resolve_path(&params.path, &base);
    let destination = params
        .destination
        .as_deref()
        .map(|d| resolve_path(d, &base));

    if !sensitive_files_allowed()
        && matches!(
            params.mode,
            FileSystemMode::Read
                | FileSystemMode::Write
                | FileSystemMode::Edit
                | FileSystemMode::Copy
                | FileSystemMode::Move
                | FileSystemMode::Delete
        )
    {
        if is_sensitive_path(&path) {
            return sensitive_denied(&path);
        }
        if matches!(params.mode, FileSystemMode::Copy | FileSystemMode::Move)
            && let Some(destination) = destination.as_deref()
            && is_sensitive_path(destination)
        {
            return sensitive_denied(destination);
        }
    }

    match params.mode {
        FileSystemMode::Read => read_file(&path, params.offset, params.limit, encoding),
        FileSystemMode::Write => match params.content {
            None => "Error: content parameter is required for write mode.".to_string(),
            Some(content) => write_file(&path, &content, append, encoding),
        },
        FileSystemMode::Edit => match (params.old_text, params.new_text) {
            (Some(old_text), Some(new_text)) => edit_file(&path, &old_text, &new_text, encoding),
            _ => "Error: old_text and new_text parameters are required for edit mode.".to_string(),
        },
        FileSystemMode::Copy => match destination {
            None => "Error: destination parameter is required for copy mode.".to_string(),
            Some(dst) => copy_path(&path, &dst, overwrite),
        },
        FileSystemMode::Move => match destination {
            None => "Error: destination parameter is required for move mode.".to_string(),
            Some(dst) => move_path(&path, &dst, overwrite),
        },
        FileSystemMode::Delete => delete_path(&path, recursive, dry_run),
        FileSystemMode::List => {
            list_directory(&path, params.pattern.as_deref(), recursive, show_hidden)
        }
        FileSystemMode::Search => match params.pattern {
            None => "Error: pattern parameter is required for search mode.".to_string(),
            Some(pattern) => search_files(
                &path,
                &pattern,
                params.content_pattern.as_deref(),
                recursive,
                encoding,
            ),
        },
        FileSystemMode::Info => get_file_info(&path),
    }
}

fn sensitive_files_allowed() -> bool {
    std::env::var("WINDOWS_MCP_ALLOW_SENSITIVE_FILES").as_deref() == Ok("1")
}

fn is_sensitive_path(path: &Path) -> bool {
    sensitive_name(path) || fs::canonicalize(path).is_ok_and(|resolved| sensitive_name(&resolved))
}

fn sensitive_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name.starts_with(".env")
        || name.starts_with("id_rsa")
        || name.starts_with("credentials")
        || ["pem", "key", "pfx"].iter().any(|extension| {
            path.extension()
                .is_some_and(|value| value.eq_ignore_ascii_case(extension))
        })
}

fn sensitive_denied(path: &Path) -> String {
    format!(
        "Error: access to sensitive file denied: {}. Set WINDOWS_MCP_ALLOW_SENSITIVE_FILES=1 to allow.",
        path.display()
    )
}

/// Resolves the user's Desktop directory, falling back to the current
/// working directory if it cannot be determined.
fn desktop_dir() -> PathBuf {
    dirs::desktop_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
}

/// Resolves `path` against `base` if it is not already absolute.
fn resolve_path(path: &str, base: &Path) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

/// Appends a Windows-elevation hint when the current process is not elevated.
fn permission_hint() -> &'static str {
    if is_elevated() {
        ""
    } else {
        "\n\nHINT: This operation may require an elevated (Administrator) terminal."
    }
}

fn is_elevated() -> bool {
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

fn read_file(
    path: &Path,
    offset: Option<i64>,
    limit: Option<i64>,
    encoding: &'static encoding_rs::Encoding,
) -> String {
    if !path.exists() {
        return format!("Error: File not found: {}", path.display());
    }
    if !path.is_file() {
        return format!("Error: Path is not a file: {}", path.display());
    }

    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return read_error(&e, path),
    };
    if metadata.len() > MAX_READ_SIZE {
        return format!(
            "Error: File too large ({} bytes). Maximum is {} bytes. Use offset/limit parameters or the Shell tool for large files.",
            with_commas(metadata.len()),
            with_commas(MAX_READ_SIZE)
        );
    }

    let bytes = match read_bounded_file(path) {
        Ok(b) => b,
        Err(e) => return read_error(&e, path),
    };
    let text = decode_text(&bytes, encoding);

    if offset.is_some() || limit.is_some() {
        let lines: Vec<&str> = split_keep_newlines(&text);
        let total = lines.len();
        let requested_offset = offset.unwrap_or(1);
        let start = if requested_offset < 0 {
            total.saturating_sub(requested_offset.unsigned_abs() as usize)
        } else {
            (requested_offset - 1).max(0) as usize
        };
        let end = match limit {
            Some(l) => start.saturating_add(l.max(0) as usize),
            None => total,
        };
        let end_clamped = end.min(total);
        let selected: String = lines.get(start..end_clamped).unwrap_or(&[]).concat();
        format!(
            "File: {}\nLines {}-{} of {}:\n{}",
            path.display(),
            start + 1,
            end_clamped,
            total,
            selected
        )
    } else {
        format!("File: {}\n{}", path.display(), text)
    }
}

fn edit_file(
    path: &Path,
    old_text: &str,
    new_text: &str,
    encoding: &'static encoding_rs::Encoding,
) -> String {
    if old_text.is_empty() {
        return "Error: old_text must not be empty.".to_string();
    }
    if !path.is_file() {
        return format!("Error: File not found: {}", path.display());
    }
    let bytes = match read_bounded_file(path) {
        Ok(bytes) => bytes,
        Err(error) => return read_error(&error, path),
    };
    if bytes.len() as u64 > MAX_READ_SIZE {
        return format!(
            "Error: File too large ({} bytes).",
            with_commas(bytes.len() as u64)
        );
    }
    let (actual_encoding, bom_len) =
        encoding_rs::Encoding::for_bom(&bytes).unwrap_or((encoding, 0));
    let text = match decode_text_checked(&bytes[bom_len..], actual_encoding) {
        Ok(text) => text,
        Err(error) => return format!("Error: {error}; file was not edited."),
    };
    let count = text.match_indices(old_text).count();
    if count == 0 {
        let closest = closest_line(&text, old_text)
            .map(|(number, line)| format!(" Closest line: {number}: {line}"))
            .unwrap_or_default();
        return format!(
            "Error: old_text was not found in {}.{closest}",
            path.display()
        );
    }
    if count > 1 {
        return format!(
            "Error: old_text matched {count} occurrences in {}; expected exactly one.",
            path.display()
        );
    }
    let updated = text.replacen(old_text, new_text, 1);
    let encoded = match encode_text_checked(&updated, actual_encoding) {
        Ok(bytes) => bytes,
        Err(error) => return format!("Error: {error}; file was not edited."),
    };
    let mut output = bytes[..bom_len].to_vec();
    output.extend_from_slice(encoded.as_ref());
    match fs::write(path, output) {
        Ok(()) => format!("Edited {} (1 replacement)", path.display()),
        Err(error) => write_error(&error, path),
    }
}

fn closest_line<'a>(text: &'a str, needle: &str) -> Option<(usize, &'a str)> {
    text.lines()
        .enumerate()
        .max_by(|(_, left), (_, right)| {
            strsim::normalized_levenshtein(left, needle)
                .total_cmp(&strsim::normalized_levenshtein(right, needle))
        })
        .map(|(index, line)| (index + 1, line))
}

/// Splits `text` into lines, keeping the trailing newline on each (mirrors
/// Python's `str.readlines()`), so line ranges can be reassembled verbatim.
fn split_keep_newlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, c) in text.char_indices() {
        if c == '\n' {
            lines.push(&text[start..=i]);
            start = i + 1;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

fn read_error(e: &io::Error, path: &Path) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "Error: Permission denied: {}{}",
            path.display(),
            permission_hint()
        )
    } else {
        format!("Error reading file: {e}")
    }
}

fn write_file(
    path: &Path,
    content: &str,
    append: bool,
    encoding: &'static encoding_rs::Encoding,
) -> String {
    if let Some(parent) = path.parent()
        && let Err(e) = fs::create_dir_all(parent)
    {
        return write_error(&e, path);
    }

    let encoded = match encode_text_checked(content, encoding) {
        Ok(bytes) => bytes,
        Err(error) => return format!("Error: {error}; file was not written."),
    };
    let result = if append {
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| {
                use std::io::Write;
                f.write_all(encoded.as_ref())
            })
    } else {
        fs::write(path, encoded.as_ref())
    };

    if let Err(e) = result {
        return write_error(&e, path);
    }

    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let action = if append { "Appended to" } else { "Written to" };
    format!("{action} {} ({} bytes)", path.display(), with_commas(size))
}

fn decode_text(bytes: &[u8], encoding: &'static encoding_rs::Encoding) -> String {
    if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
        let units = bytes.chunks_exact(2).map(|pair| {
            if encoding == encoding_rs::UTF_16LE {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        });
        String::from_utf16_lossy(&units.collect::<Vec<_>>())
    } else {
        encoding.decode(bytes).0.into_owned()
    }
}

fn decode_text_checked(
    bytes: &[u8],
    encoding: &'static encoding_rs::Encoding,
) -> Result<String, String> {
    if encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE {
        if !bytes.len().is_multiple_of(2) {
            return Err("Invalid UTF-16 byte length".to_string());
        }
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| {
                if encoding == encoding_rs::UTF_16LE {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|error| format!("Invalid UTF-16: {error}"))
    } else {
        let (text, errors) = encoding.decode_without_bom_handling(bytes);
        if errors {
            Err(format!("Invalid bytes for {}", encoding.name()))
        } else {
            Ok(text.into_owned())
        }
    }
}

fn encode_text_checked<'a>(
    content: &'a str,
    encoding: &'static encoding_rs::Encoding,
) -> Result<std::borrow::Cow<'a, [u8]>, String> {
    if encoding == encoding_rs::UTF_16LE {
        Ok(std::borrow::Cow::Owned(
            content.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        ))
    } else if encoding == encoding_rs::UTF_16BE {
        Ok(std::borrow::Cow::Owned(
            content.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        ))
    } else {
        let (bytes, _, errors) = encoding.encode(content);
        if errors {
            Err(format!("Text cannot be represented in {}", encoding.name()))
        } else {
            Ok(bytes)
        }
    }
}

fn write_error(e: &io::Error, path: &Path) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "Error: Permission denied: {}{}",
            path.display(),
            permission_hint()
        )
    } else {
        format!("Error writing file: {e}")
    }
}

fn comparable_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "Invalid destination path")
                })?
                .to_os_string(),
        );
        ancestor = ancestor.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "No existing path ancestor")
        })?;
    }
    let mut resolved = fs::canonicalize(ancestor)?;
    for component in suffix.into_iter().rev() {
        resolved.push(component);
    }
    Ok(PathBuf::from(resolved.to_string_lossy().to_lowercase()))
}

fn validate_transfer(src: &Path, dst: &Path) -> io::Result<()> {
    let source = comparable_path(src)?;
    let destination = comparable_path(dst)?;
    if source == destination
        || source.starts_with(&destination)
        || (src.is_dir() && destination.starts_with(&source))
        || (dst.exists() && same_file::is_same_file(src, dst)?)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Source and destination must not be the same path or contain one another",
        ));
    }
    Ok(())
}

/// An exclusively created sibling directory owns temporary copies and backups.
struct TransferStage(PathBuf);

impl TransferStage {
    fn new(destination: &Path) -> io::Result<Self> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let parent = destination.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Destination has no parent")
        })?;
        fs::create_dir_all(parent)?;
        loop {
            let path = parent.join(format!(
                ".woc-transfer-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for TransferStage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn replace_destination(source: &Path, destination: &Path, overwrite: bool) -> io::Result<()> {
    if destination.exists() && !overwrite {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Destination already exists; set overwrite=True to replace",
        ));
    }
    if !destination.exists() || (source.is_file() && destination.is_file()) {
        return fs::rename(source, destination);
    }
    let backup = TransferStage::new(destination)?;
    let saved = backup.0.join("previous");
    fs::rename(destination, &saved)?;
    if let Err(error) = fs::rename(source, destination) {
        if let Err(restore) = fs::rename(&saved, destination) {
            // Preserve the only remaining original if rollback itself fails.
            let location = saved.display().to_string();
            std::mem::forget(backup);
            return Err(io::Error::other(format!(
                "{error}; rollback failed: {restore}; original retained at {location}"
            )));
        }
        return Err(error);
    }
    Ok(())
}

fn copy_path(src: &Path, dst: &Path, overwrite: bool) -> String {
    if !src.exists() {
        return format!("Error: Source not found: {}", src.display());
    }
    if let Err(error) = validate_transfer(src, dst) {
        return copy_error(&error);
    }
    if dst.exists() && !overwrite {
        return format!(
            "Error: Destination already exists: {}. Set overwrite=True to replace.",
            dst.display()
        );
    }
    let result = (|| {
        let stage = TransferStage::new(dst)?;
        let staged = stage.0.join("new");
        if src.is_dir() {
            copy_dir_recursive(src, &staged)?;
        } else if src.is_file() {
            copy_file(src, &staged)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unsupported file type",
            ));
        }
        replace_destination(&staged, dst, overwrite)
    })();
    match result {
        Ok(()) => format!(
            "Copied {}: {} -> {}",
            if src.is_dir() { "directory" } else { "file" },
            src.display(),
            dst.display()
        ),
        Err(error) => copy_error(&error),
    }
}

fn copy_error(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!("Error: Permission denied.{}", permission_hint())
    } else {
        format!("Error copying: {e}")
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            copy_file(&entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

fn copy_file(src: &Path, dst: &Path) -> io::Result<()> {
    fs::copy(src, dst)?;
    let metadata = fs::metadata(src)?;
    fs::set_permissions(dst, metadata.permissions())?;
    let accessed = filetime::FileTime::from_last_access_time(&metadata);
    let modified = filetime::FileTime::from_last_modification_time(&metadata);
    filetime::set_file_times(dst, accessed, modified)
}

fn move_path(src: &Path, dst: &Path, overwrite: bool) -> String {
    if !src.exists() {
        return format!("Error: Source not found: {}", src.display());
    }
    if let Err(error) = validate_transfer(src, dst) {
        return move_error(&error);
    }
    let result = (|| {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        match replace_destination(src, dst, overwrite) {
            Ok(()) => Ok(()),
            // ERROR_NOT_SAME_DEVICE: only cross-volume moves use copy/delete.
            Err(error) if error.raw_os_error() == Some(17) => {
                let stage = TransferStage::new(dst)?;
                let staged = stage.0.join("new");
                if src.is_dir() {
                    copy_dir_recursive(src, &staged)?;
                } else {
                    copy_file(src, &staged)?;
                }
                replace_destination(&staged, dst, overwrite)?;
                if src.is_dir() {
                    fs::remove_dir_all(src)
                } else {
                    fs::remove_file(src)
                }
            }
            Err(error) => Err(error),
        }
    })();
    match result {
        Ok(()) => format!("Moved: {} -> {}", src.display(), dst.display()),
        Err(error) => move_error(&error),
    }
}

fn move_error(e: &io::Error) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!("Error: Permission denied.{}", permission_hint())
    } else {
        format!("Error moving: {e}")
    }
}

fn delete_path(path: &Path, recursive: bool, dry_run: bool) -> String {
    if !path.exists() {
        return format!("Error: Path not found: {}", path.display());
    }

    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => return delete_error(&e, path),
    };

    if dry_run {
        let mut targets = vec![path.to_path_buf()];
        if meta.is_dir() && recursive {
            match walk(path, true, false) {
                Ok(walked) if walked.skipped == 0 && !walked.truncated => targets.extend(walked.entries.into_iter().map(|(path, _)| path)),
                Ok(_) => return "Error: Delete preview could not enumerate every target; nothing was removed.".to_string(),
                Err(error) => return delete_error(&error, path),
            }
        }
        let total = targets.len();
        let mut lines: Vec<String> = targets
            .iter()
            .take(MAX_RESULTS)
            .map(|target| target.display().to_string())
            .collect();
        if total > MAX_RESULTS {
            lines.push(format!("... (truncated, {MAX_RESULTS}+ targets)"));
        }
        return format!(
            "Dry run: would delete {total} target(s):\n{}",
            lines.join("\n")
        );
    }

    if meta.is_file() || meta.file_type().is_symlink() {
        match fs::remove_file(path) {
            Ok(()) => format!("Deleted file: {}", path.display()),
            Err(e) => delete_error(&e, path),
        }
    } else if meta.is_dir() {
        if !recursive {
            match fs::read_dir(path) {
                Ok(mut entries) => {
                    if entries.next().is_some() {
                        return format!(
                            "Error: Directory is not empty: {}. Set recursive=True to delete non-empty directories.",
                            path.display()
                        );
                    }
                }
                Err(e) => return delete_error(&e, path),
            }
            match fs::remove_dir(path) {
                Ok(()) => format!("Deleted directory: {}", path.display()),
                Err(e) => delete_error(&e, path),
            }
        } else {
            match fs::remove_dir_all(path) {
                Ok(()) => format!("Deleted directory: {}", path.display()),
                Err(e) => delete_error(&e, path),
            }
        }
    } else {
        format!("Error: Unsupported file type: {}", path.display())
    }
}

fn delete_error(e: &io::Error, path: &Path) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "Error: Permission denied: {}{}",
            path.display(),
            permission_hint()
        )
    } else {
        format!("Error deleting: {e}")
    }
}

struct Entry {
    is_dir: bool,
    size: u64,
    relative: String,
}

impl Entry {
    fn to_line(&self) -> String {
        let entry_type = if self.is_dir { "DIR " } else { "FILE" };
        let size_str = if self.is_dir {
            String::new()
        } else {
            format_size(self.size)
        };
        format!("  [{entry_type}] {}  {size_str}", self.relative)
    }
}

/// Lowercased file name used as a sort key (dirs-first, case-insensitive).
fn sort_key(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn list_directory(
    path: &Path,
    pattern: Option<&str>,
    recursive: bool,
    show_hidden: bool,
) -> String {
    if !path.exists() {
        return format!("Error: Directory not found: {}", path.display());
    }
    if !path.is_dir() {
        return format!("Error: Path is not a directory: {}", path.display());
    }

    let walked = match walk(path, recursive, !show_hidden) {
        Ok(v) => v,
        Err(e) => return list_error(&e, path),
    };
    let notice = walked.notice();
    let mut raw = walked.entries;
    raw.retain(|(entry_path, _)| {
        let name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if !show_hidden && name.starts_with('.') {
            return false;
        }
        match pattern {
            Some(p) => glob_match(p, name),
            None => true,
        }
    });
    raw.sort_by(|(a, a_is_dir), (b, b_is_dir)| {
        (!a_is_dir)
            .cmp(&!b_is_dir)
            .then_with(|| sort_key(a).cmp(&sort_key(b)))
    });

    if raw.is_empty() {
        let filter_msg = pattern
            .map(|p| format!(" matching \"{p}\""))
            .unwrap_or_default();
        return format!("Directory {} is empty{filter_msg}.{notice}", path.display());
    }

    let mut lines = Vec::new();
    let total = raw.len();
    for (entry_path, is_dir) in raw.iter().take(MAX_RESULTS) {
        let size = if *is_dir {
            0
        } else {
            fs::metadata(entry_path).map(|m| m.len()).unwrap_or(0)
        };
        let relative = if recursive {
            entry_path
                .strip_prefix(path)
                .unwrap_or(entry_path)
                .display()
                .to_string()
        } else {
            entry_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string()
        };
        lines.push(
            Entry {
                is_dir: *is_dir,
                size,
                relative,
            }
            .to_line(),
        );
    }
    if total > MAX_RESULTS {
        lines.push(format!("... (truncated, {MAX_RESULTS}+ items)"));
    }

    let mut header = format!("Directory: {}", path.display());
    if let Some(p) = pattern {
        header.push_str(&format!(" (filter: {p})"));
    }
    format!("{header}\n{}{notice}", lines.join("\n"))
}

fn list_error(e: &io::Error, path: &Path) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "Error: Permission denied: {}{}",
            path.display(),
            permission_hint()
        )
    } else {
        format!("Error listing directory: {e}")
    }
}

fn search_files(
    path: &Path,
    pattern: &str,
    content_pattern: Option<&str>,
    recursive: bool,
    encoding: &'static encoding_rs::Encoding,
) -> String {
    if !path.exists() {
        return format!("Error: Search path not found: {}", path.display());
    }
    if !path.is_dir() {
        return format!("Error: Search path is not a directory: {}", path.display());
    }

    let walked = match walk(path, recursive, false) {
        Ok(v) => v,
        Err(e) => return search_error(&e, path),
    };
    let notice = walked.notice();
    let mut raw = walked.entries;
    raw.retain(|(entry_path, _)| {
        let name = entry_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        glob_match(pattern, name)
    });
    raw.sort_by(|(a, a_is_dir), (b, b_is_dir)| {
        (!a_is_dir)
            .cmp(&!b_is_dir)
            .then_with(|| sort_key(a).cmp(&sort_key(b)))
    });

    if let Some(content_pattern) = content_pattern {
        let regex = match regex::Regex::new(content_pattern) {
            Ok(regex) => regex,
            Err(error) => return format!("Error: Invalid content_pattern regex: {error}"),
        };
        let mut matches = Vec::new();
        let mut total = 0usize;
        let mut skipped = 0usize;
        for (entry_path, is_dir) in raw {
            if is_dir {
                continue;
            }
            if !sensitive_files_allowed() && is_sensitive_path(&entry_path) {
                skipped += 1;
                continue;
            }
            let Ok(bytes) = read_bounded_file(&entry_path) else {
                skipped += 1;
                continue;
            };
            let text = decode_text(&bytes, encoding);
            for (index, line) in text.lines().enumerate() {
                if regex.is_match(line) {
                    total += 1;
                    if matches.len() < MAX_RESULTS {
                        let preview: String = line.chars().take(1000).collect();
                        let suffix = if preview.len() < line.len() {
                            " [line truncated]"
                        } else {
                            ""
                        };
                        matches.push(format!(
                            "{}:{}: {preview}{suffix}",
                            entry_path.display(),
                            index + 1
                        ));
                    }
                }
            }
        }
        let skipped_notice = if skipped > 0 {
            format!("\nSkipped {skipped} file(s): sensitive, too large, or unreadable.")
        } else {
            String::new()
        };
        if total == 0 {
            return format!(
                "No content matches found for /{content_pattern}/ in files matching \"{pattern}\"{notice}{skipped_notice}"
            );
        }
        if total > MAX_RESULTS {
            matches.push(format!("... (truncated, {MAX_RESULTS}+ matches)"));
        }
        return format!("{}{notice}{skipped_notice}", matches.join("\n"));
    }

    if raw.is_empty() {
        return format!(
            "No matches found for \"{pattern}\" in {}{notice}",
            path.display()
        );
    }

    let total = raw.len();
    let mut lines = Vec::new();
    for (entry_path, is_dir) in raw.iter().take(MAX_RESULTS) {
        let size = if *is_dir {
            0
        } else {
            fs::metadata(entry_path).map(|m| m.len()).unwrap_or(0)
        };
        let relative = entry_path
            .strip_prefix(path)
            .unwrap_or(entry_path)
            .display()
            .to_string();
        lines.push(
            Entry {
                is_dir: *is_dir,
                size,
                relative,
            }
            .to_line(),
        );
    }
    if total > MAX_RESULTS {
        lines.push(format!("... (truncated, {MAX_RESULTS}+ matches)"));
    }

    format!(
        "Search: \"{pattern}\" in {} ({} matches)\n{}{notice}",
        path.display(),
        total.min(MAX_RESULTS),
        lines.join("\n")
    )
}

fn search_error(e: &io::Error, path: &Path) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "Error: Permission denied: {}{}",
            path.display(),
            permission_hint()
        )
    } else {
        format!("Error searching: {e}")
    }
}

/// Walks `dir`, returning `(path, is_dir)` for every entry. Recurses into
/// subdirectories when `recursive` is set.
struct WalkResult {
    entries: Vec<(PathBuf, bool)>,
    skipped: usize,
    truncated: bool,
}

impl WalkResult {
    fn notice(&self) -> String {
        let mut notice = String::new();
        if self.skipped > 0 {
            notice += &format!(
                "\nSkipped {} unreadable entries or directories.",
                self.skipped
            );
        }
        if self.truncated {
            notice += &format!("\nScan truncated at {MAX_SCAN_ENTRIES} entries.");
        }
        notice
    }
}

fn walk(dir: &Path, recursive: bool, skip_dot_dirs: bool) -> io::Result<WalkResult> {
    let mut result = WalkResult {
        entries: Vec::new(),
        skipped: 0,
        truncated: false,
    };
    let mut stack = vec![dir.to_path_buf()];
    'scan: while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(error) if current == dir => return Err(error),
            Err(_) => {
                result.skipped += 1;
                continue;
            }
        };
        for entry in entries {
            if result.entries.len() >= MAX_SCAN_ENTRIES {
                result.truncated = true;
                break 'scan;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => {
                    result.skipped += 1;
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(kind) => kind,
                Err(_) => {
                    result.skipped += 1;
                    continue;
                }
            };
            let is_dir = file_type.is_dir();
            let hidden_dir =
                skip_dot_dirs && is_dir && entry.file_name().to_string_lossy().starts_with('.');
            result.entries.push((entry.path(), is_dir));
            if recursive && is_dir && !hidden_dir {
                stack.push(entry.path());
            }
        }
    }
    Ok(result)
}

fn read_bounded_file(path: &Path) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_READ_SIZE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_READ_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "File too large (maximum 10 MiB)",
        ));
    }
    Ok(bytes)
}

fn get_file_info(path: &Path) -> String {
    if !path.exists() && fs::symlink_metadata(path).is_err() {
        return format!("Error: Path not found: {}", path.display());
    }

    let (file_type, metadata) = match fs::metadata(path) {
        Ok(m) if m.is_dir() => ("Directory", m),
        Ok(m) => ("File", m),
        Err(_) => match fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => ("Symlink", m),
            Ok(m) => ("Other", m),
            Err(e) => return info_error(&e, path),
        },
    };

    let size = metadata.len();
    let created = format_time(metadata.created());
    let modified = format_time(metadata.modified());
    let accessed = format_time(metadata.accessed());
    let read_only = metadata.permissions().readonly();

    let mut lines = vec![
        format!("Path: {}", path.display()),
        format!("Type: {file_type}"),
        format!("Size: {} ({} bytes)", format_size(size), with_commas(size)),
        format!("Created: {created}"),
        format!("Modified: {modified}"),
        format!("Accessed: {accessed}"),
        format!("Read-only: {}", if read_only { "True" } else { "False" }),
    ];

    if file_type == "Directory"
        && let Ok(entries) = fs::read_dir(path)
    {
        let mut files = 0;
        let mut dirs = 0;
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(t) if t.is_dir() => dirs += 1,
                Ok(_) => files += 1,
                Err(_) => {}
            }
        }
        lines.push(format!("Contents: {files} files, {dirs} directories"));
    }

    if file_type == "File" {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_else(|| "(none)".to_string());
        lines.push(format!("Extension: {ext}"));
    }

    if file_type == "Symlink"
        && let Ok(target) = fs::read_link(path)
    {
        lines.push(format!("Link target: {}", target.display()));
    }

    lines.join("\n")
}

fn info_error(e: &io::Error, path: &Path) -> String {
    if e.kind() == io::ErrorKind::PermissionDenied {
        format!(
            "Error: Permission denied: {}{}",
            path.display(),
            permission_hint()
        )
    } else {
        format!("Error getting file info: {e}")
    }
}

fn format_time(result: io::Result<SystemTime>) -> String {
    match result {
        Ok(t) => chrono::DateTime::<chrono::Local>::from(t)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => "unknown".to_string(),
    }
}

/// Formats a byte count as a human-readable size (`B`/`KB`/`MB`/`GB`).
fn format_size(size_bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let bytes = size_bytes as f64;
    if size_bytes < 1024 {
        format!("{size_bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes / KB)
    } else if bytes < GB {
        format!("{:.1} MB", bytes / MB)
    } else {
        format!("{:.1} GB", bytes / GB)
    }
}

/// Formats an integer with thousands separators (e.g. `12,345,678`).
fn with_commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out.chars().rev().collect()
}

/// Matches `name` against a simple glob `pattern` supporting `*` and `?`.
fn glob_match(pattern: &str, name: &str) -> bool {
    fn matches(pattern: &[char], name: &[char]) -> bool {
        match (pattern.first(), name.first()) {
            (None, None) => true,
            (Some('*'), _) => {
                matches(&pattern[1..], name) || (!name.is_empty() && matches(pattern, &name[1..]))
            }
            (Some('?'), Some(_)) => matches(&pattern[1..], &name[1..]),
            (Some(p), Some(n)) if p.eq_ignore_ascii_case(n) => matches(&pattern[1..], &name[1..]),
            _ => false,
        }
    }
    let pattern_chars: Vec<char> = pattern.chars().collect();
    let name_chars: Vec<char> = name.chars().collect();
    matches(&pattern_chars, &name_chars)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_formatting() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn commas() {
        assert_eq!(with_commas(0), "0");
        assert_eq!(with_commas(999), "999");
        assert_eq!(with_commas(1000), "1,000");
        assert_eq!(with_commas(12_345_678), "12,345,678");
    }

    #[test]
    fn relative_path_resolution() {
        let base = Path::new(r"C:\Users\test\Desktop");
        assert_eq!(resolve_path("notes.txt", base), base.join("notes.txt"));
        assert_eq!(
            resolve_path(r"C:\absolute\path.txt", base),
            PathBuf::from(r"C:\absolute\path.txt")
        );
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("*.txt", "notes.txt"));
        assert!(!glob_match("*.txt", "notes.md"));
        assert!(glob_match("file?.log", "file1.log"));
        assert!(glob_match("*", "anything"));
    }

    #[test]
    fn reads_and_writes_requested_text_encoding() {
        let path = std::env::temp_dir().join(format!(
            "windows-operation-cli-encoding-test-{}.txt",
            std::process::id()
        ));
        let encoding = encoding_rs::UTF_16LE;
        let written = write_file(&path, "hello 世界", false, encoding);
        assert!(written.starts_with("Written to"));
        let read = read_file(&path, None, None, encoding);
        assert!(read.ends_with("hello 世界"));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn negative_offset_reads_from_end() {
        let path = std::env::temp_dir().join(format!(
            "windows-operation-cli-tail-test-{}.txt",
            std::process::id()
        ));
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let result = read_file(&path, Some(-2), None, encoding_rs::UTF_8);
        assert!(result.ends_with("two\nthree\n"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn edit_requires_exactly_one_match() {
        let path = std::env::temp_dir().join(format!(
            "windows-operation-cli-edit-test-{}.txt",
            std::process::id()
        ));
        fs::write(&path, "alpha\nbeta\n").unwrap();
        assert!(edit_file(&path, "beta", "gamma", encoding_rs::UTF_8).starts_with("Edited"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "alpha\ngamma\n");
        let missing = edit_file(&path, "gama", "delta", encoding_rs::UTF_8);
        assert!(missing.contains("Closest line: 2: gamma"));
        fs::write(&path, "same same").unwrap();
        assert!(edit_file(&path, "same", "new", encoding_rs::UTF_8).contains("2 occurrences"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn delete_dry_run_preserves_targets() {
        let dir = std::env::temp_dir().join(format!(
            "windows-operation-cli-delete-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("keep.txt"), "keep").unwrap();
        let result = delete_path(&dir, true, true);
        assert!(result.starts_with("Dry run:"));
        assert!(dir.join("keep.txt").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn content_search_reports_path_and_line() {
        let dir = std::env::temp_dir().join(format!(
            "windows-operation-cli-search-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("notes.txt"), "alpha\nerror 42\n").unwrap();
        let result = search_files(&dir, "*.txt", Some(r"error \d+"), false, encoding_rs::UTF_8);
        assert!(result.contains("notes.txt:2: error 42"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sensitive_file_names_are_matched_case_insensitively() {
        assert!(is_sensitive_path(Path::new(".env.local")));
        assert!(is_sensitive_path(Path::new("ID_RSA.pub")));
        assert!(is_sensitive_path(Path::new("server.PEM")));
        assert!(is_sensitive_path(Path::new("credentials.json")));
        assert!(!is_sensitive_path(Path::new("environment.txt")));
    }
}
