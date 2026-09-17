//! The `FileSystem` tool against a directory this test owns.
//!
//! Several modes write and delete, so everything here happens inside a
//! throwaway directory removed when the test ends. Nothing outside it is
//! touched, and no test uses a relative path — those resolve against the
//! user's Desktop.

#![cfg(target_os = "windows")]

use std::path::{Path, PathBuf};

use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::tools::filesystem::{FileSystemMode, FileSystemParams, file_system};

/// A directory under the system temp folder, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "wocli-fs-{name}-{}-{unique}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("could not create the scratch directory");
        Self(path)
    }

    fn join(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }

    fn write(&self, name: &str, contents: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, contents).expect("could not seed a file");
        path.display().to_string()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn params(mode: FileSystemMode, path: &str) -> FileSystemParams {
    FileSystemParams {
        mode,
        path: path.to_string(),
        destination: None,
        content: None,
        old_text: None,
        new_text: None,
        pattern: None,
        content_pattern: None,
        recursive: None,
        append: None,
        overwrite: None,
        offset: None,
        limit: None,
        encoding: "utf-8".to_string(),
        show_hidden: None,
        dry_run: None,
    }
}

fn assert_ok(response: &str, what: &str) {
    assert!(
        !response.to_lowercase().starts_with("error"),
        "{what} failed: {response}"
    );
}

#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn written_text_reads_back_verbatim() {
    let scratch = Scratch::new("write");
    let path = scratch.join("note.txt");
    let text = "line one\n日本語の行\nline three";

    let written = file_system(FileSystemParams {
        content: Some(text.to_string()),
        ..params(FileSystemMode::Write, &path)
    });
    assert_ok(&written, "write");

    let read = file_system(params(FileSystemMode::Read, &path));
    assert!(read.contains("日本語の行"), "the read lost content: {read}");
    assert!(read.contains("line three"), "the read was truncated: {read}");
}

/// `append` has to add to the file, not replace it.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn appending_keeps_what_was_there() {
    let scratch = Scratch::new("append");
    let path = scratch.write("log.txt", "first\n");

    let appended = file_system(FileSystemParams {
        content: Some("second\n".to_string()),
        append: Some(BoolOrString::Bool(true)),
        ..params(FileSystemMode::Write, &path)
    });
    assert_ok(&appended, "append");

    let contents = std::fs::read_to_string(&path).expect("read back");
    assert_eq!(contents, "first\nsecond\n");
}

/// `offset`/`limit` select a window of lines rather than the whole file.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn reading_a_range_returns_only_those_lines() {
    let scratch = Scratch::new("range");
    let body: String = (1..=10).map(|n| format!("line {n}\n")).collect();
    let path = scratch.write("many.txt", &body);

    let read = file_system(FileSystemParams {
        offset: Some(3),
        limit: Some(2),
        ..params(FileSystemMode::Read, &path)
    });
    assert!(read.contains("line 3"), "the window missed its start: {read}");
    assert!(read.contains("line 4"), "the window missed its end: {read}");
    assert!(
        !read.contains("line 6"),
        "the window ran past its limit: {read}"
    );
}

/// A non-UTF-8 encoding has to be honoured on the way in and out.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn a_shift_jis_file_round_trips() {
    let scratch = Scratch::new("sjis");
    let path = scratch.join("sjis.txt");
    let text = "日本語テキスト";

    let written = file_system(FileSystemParams {
        content: Some(text.to_string()),
        encoding: "shift_jis".to_string(),
        ..params(FileSystemMode::Write, &path)
    });
    assert_ok(&written, "shift_jis write");

    // The bytes on disk must not be UTF-8: that is what proves the encoding
    // was applied rather than ignored.
    let bytes = std::fs::read(&path).expect("read bytes");
    assert_ne!(
        bytes,
        text.as_bytes(),
        "the file was written as UTF-8 despite the shift_jis label"
    );

    let read = file_system(FileSystemParams {
        encoding: "shift_jis".to_string(),
        ..params(FileSystemMode::Read, &path)
    });
    assert!(read.contains(text), "the shift_jis file did not read back: {read}");
}

/// `edit` replaces one exact occurrence and leaves the rest alone.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn editing_replaces_the_named_text() {
    let scratch = Scratch::new("edit");
    let path = scratch.write("config.txt", "host = localhost\nport = 8080\n");

    let edited = file_system(FileSystemParams {
        old_text: Some("8080".to_string()),
        new_text: Some("9090".to_string()),
        ..params(FileSystemMode::Edit, &path)
    });
    assert_ok(&edited, "edit");

    let contents = std::fs::read_to_string(&path).expect("read back");
    assert_eq!(contents, "host = localhost\nport = 9090\n");
}

/// Text that appears twice is ambiguous: replacing one at random would be a
/// silent wrong answer.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn editing_ambiguous_text_is_refused() {
    let scratch = Scratch::new("ambiguous");
    let path = scratch.write("dup.txt", "value\nvalue\n");

    let response = file_system(FileSystemParams {
        old_text: Some("value".to_string()),
        new_text: Some("changed".to_string()),
        ..params(FileSystemMode::Edit, &path)
    });
    assert!(
        response.to_lowercase().contains("error"),
        "two matches should be refused: {response}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("read back"),
        "value\nvalue\n",
        "a refused edit must leave the file alone"
    );
}

/// Copy leaves the source in place; move does not.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn copy_keeps_the_source_and_move_does_not() {
    let scratch = Scratch::new("copymove");
    let source = scratch.write("source.txt", "payload");
    let copy = scratch.join("copy.txt");
    let moved = scratch.join("moved.txt");

    let copied = file_system(FileSystemParams {
        destination: Some(copy.clone()),
        ..params(FileSystemMode::Copy, &source)
    });
    assert_ok(&copied, "copy");
    assert!(Path::new(&source).exists(), "copy removed the source");
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "payload");

    let moved_response = file_system(FileSystemParams {
        destination: Some(moved.clone()),
        ..params(FileSystemMode::Move, &copy)
    });
    assert_ok(&moved_response, "move");
    assert!(!Path::new(&copy).exists(), "move left the source behind");
    assert_eq!(std::fs::read_to_string(&moved).unwrap(), "payload");
}

/// Overwriting an existing destination needs saying so.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn copying_onto_an_existing_file_needs_overwrite() {
    let scratch = Scratch::new("overwrite");
    let source = scratch.write("from.txt", "new");
    let destination = scratch.write("to.txt", "existing");

    let refused = file_system(FileSystemParams {
        destination: Some(destination.clone()),
        ..params(FileSystemMode::Copy, &source)
    });
    assert!(
        refused.to_lowercase().contains("error"),
        "an existing destination should be refused: {refused}"
    );
    assert_eq!(
        std::fs::read_to_string(&destination).unwrap(),
        "existing",
        "the refused copy overwrote the destination anyway"
    );

    let allowed = file_system(FileSystemParams {
        destination: Some(destination.clone()),
        overwrite: Some(BoolOrString::Bool(true)),
        ..params(FileSystemMode::Copy, &source)
    });
    assert_ok(&allowed, "copy with overwrite");
    assert_eq!(std::fs::read_to_string(&destination).unwrap(), "new");
}

/// `dry_run` reports what would go without removing it.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn a_dry_run_delete_removes_nothing() {
    let scratch = Scratch::new("dryrun");
    let path = scratch.write("doomed.txt", "still here");

    let response = file_system(FileSystemParams {
        dry_run: Some(BoolOrString::Bool(true)),
        ..params(FileSystemMode::Delete, &path)
    });
    assert_ok(&response, "dry-run delete");
    assert!(
        Path::new(&path).exists(),
        "a dry run deleted the file: {response}"
    );

    let real = file_system(params(FileSystemMode::Delete, &path));
    assert_ok(&real, "delete");
    assert!(!Path::new(&path).exists(), "the file survived deletion");
}

/// `list` shows what is in the directory; `search` narrows by glob and by
/// content.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn list_and_search_find_the_right_files() {
    let scratch = Scratch::new("listing");
    scratch.write("alpha.txt", "needle in here");
    scratch.write("beta.txt", "nothing of note");
    scratch.write("gamma.log", "needle here too");

    let listing = file_system(params(FileSystemMode::List, &scratch.join("")));
    for name in ["alpha.txt", "beta.txt", "gamma.log"] {
        assert!(listing.contains(name), "the listing missed {name}: {listing}");
    }

    let by_glob = file_system(FileSystemParams {
        pattern: Some("*.log".to_string()),
        ..params(FileSystemMode::Search, &scratch.join(""))
    });
    assert!(by_glob.contains("gamma.log"), "the glob missed its match: {by_glob}");
    assert!(
        !by_glob.contains("alpha.txt"),
        "the glob matched a file it should not have: {by_glob}"
    );

    let by_content = file_system(FileSystemParams {
        pattern: Some("*.txt".to_string()),
        content_pattern: Some("needle".to_string()),
        ..params(FileSystemMode::Search, &scratch.join(""))
    });
    assert!(
        by_content.contains("alpha.txt"),
        "the content search missed its match: {by_content}"
    );
    assert!(
        !by_content.contains("beta.txt"),
        "the content search matched a file without the text: {by_content}"
    );
}

/// A path that does not exist is a caller-facing error, not a panic.
#[test]
#[ignore = "reads the filesystem; run with --ignored"]
fn a_missing_path_is_reported() {
    let scratch = Scratch::new("missing");
    let response = file_system(params(FileSystemMode::Read, &scratch.join("nothing.txt")));
    assert!(
        response.to_lowercase().contains("error"),
        "a missing file should be reported: {response}"
    );
}

/// `info` describes the file rather than returning its contents.
#[test]
#[ignore = "writes under the system temp directory; run with --ignored"]
fn info_describes_the_file() {
    let scratch = Scratch::new("info");
    let path = scratch.write("sized.txt", "0123456789");

    let response = file_system(params(FileSystemMode::Info, &path));
    assert_ok(&response, "info");
    assert!(
        response.contains("10") || response.to_lowercase().contains("byte"),
        "info did not report a size: {response}"
    );
}
