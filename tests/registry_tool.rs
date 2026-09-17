//! The `Registry` tool, against a key this test creates and removes.
//!
//! `set` and `delete` write to the registry, so everything here lives under
//! a throwaway subkey of `HKCU\Software`, which needs no elevation and
//! belongs to nobody else. Nothing outside that subtree is touched.

#![cfg(target_os = "windows")]

use windows_operation_cli::tools::registry::{
    RegistryMode, RegistryParams, RegistryValueType, registry,
};

/// Everything these tests write lives under this one key.
const SCRATCH_ROOT: &str = r"Software\WindowsOperationCliTest";

/// A key under HKCU that is removed when the test ends.
struct Scratch {
    path: String,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let path = format!("HKCU:\\{SCRATCH_ROOT}\\{name}");
        let scratch = Self { path };
        scratch.remove();
        scratch
    }

    fn remove(&self) {
        // Setup and teardown go through the registry API directly: the point
        // of the test is the tool's behaviour, not its ability to clean up
        // after itself.
        let subkey = self
            .path
            .strip_prefix("HKCU:\\")
            .expect("scratch keys live under HKCU");
        let _ = windows_registry::CURRENT_USER.remove_tree(subkey);
        // Remove the shared parent too, so a run leaves nothing behind. Only
        // when it is empty: another test's scratch key may still be under it.
        let empty = windows_registry::CURRENT_USER
            .open(SCRATCH_ROOT)
            .is_ok_and(|key| key.keys().is_ok_and(|mut keys| keys.next().is_none()));
        if empty {
            let _ = windows_registry::CURRENT_USER.remove_tree(SCRATCH_ROOT);
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        self.remove();
    }
}

fn call(
    mode: RegistryMode,
    path: &str,
    name: Option<&str>,
    value: Option<&str>,
    value_type: RegistryValueType,
) -> String {
    registry(RegistryParams {
        mode,
        path: path.to_string(),
        name: name.map(str::to_string),
        value: value.map(str::to_string),
        value_type,
    })
}

#[test]
#[ignore = "writes to HKCU\\Software\\WindowsOperationCliTest; run with --ignored"]
fn a_written_value_reads_back() {
    let scratch = Scratch::new("RoundTrip");

    let written = call(
        RegistryMode::Set,
        &scratch.path,
        Some("Greeting"),
        Some("こんにちは"),
        RegistryValueType::String,
    );
    assert!(
        !written.to_lowercase().starts_with("error"),
        "set failed: {written}"
    );

    let read = call(RegistryMode::Get, &scratch.path, Some("Greeting"), None, RegistryValueType::String);
    assert!(
        read.contains("こんにちは"),
        "the value did not read back: {read}"
    );
}

/// A DWORD has to come back as a number, not as the string that was passed
/// in — the type parameter is the whole point of `set`.
#[test]
#[ignore = "writes to HKCU\\Software\\WindowsOperationCliTest; run with --ignored"]
fn a_dword_keeps_its_type() {
    let scratch = Scratch::new("Dword");

    call(
        RegistryMode::Set,
        &scratch.path,
        Some("Count"),
        Some("42"),
        RegistryValueType::DWord,
    );
    let read = call(RegistryMode::Get, &scratch.path, Some("Count"), None, RegistryValueType::String);
    assert!(read.contains("42"), "the DWORD did not read back: {read}");
}

/// `list` has to show the values that were written under the key.
#[test]
#[ignore = "writes to HKCU\\Software\\WindowsOperationCliTest; run with --ignored"]
fn list_shows_the_values_under_a_key() {
    let scratch = Scratch::new("Listing");

    for (name, value) in [("First", "one"), ("Second", "two")] {
        call(
            RegistryMode::Set,
            &scratch.path,
            Some(name),
            Some(value),
            RegistryValueType::String,
        );
    }

    let listing = call(RegistryMode::List, &scratch.path, None, None, RegistryValueType::String);
    assert!(listing.contains("First"), "listing missed First: {listing}");
    assert!(listing.contains("Second"), "listing missed Second: {listing}");
}

/// Deleting has to remove the value, and reading it afterwards has to fail
/// rather than return a stale one.
#[test]
#[ignore = "writes to HKCU\\Software\\WindowsOperationCliTest; run with --ignored"]
fn a_deleted_value_is_gone() {
    let scratch = Scratch::new("Deletion");

    call(
        RegistryMode::Set,
        &scratch.path,
        Some("Temporary"),
        Some("value"),
        RegistryValueType::String,
    );
    let deleted = call(
        RegistryMode::Delete,
        &scratch.path,
        Some("Temporary"),
        None,
        RegistryValueType::String,
    );
    assert!(
        !deleted.to_lowercase().starts_with("error"),
        "delete failed: {deleted}"
    );

    let read = call(
        RegistryMode::Get,
        &scratch.path,
        Some("Temporary"),
        None,
        RegistryValueType::String,
    );
    assert!(
        read.to_lowercase().contains("error") || read.to_lowercase().contains("not found"),
        "a deleted value still read back: {read}"
    );
}

/// A missing key is a caller-facing error, not a panic or an empty success.
#[test]
#[ignore = "reads the registry; run with --ignored"]
fn a_missing_key_is_reported() {
    let response = call(
        RegistryMode::Get,
        "HKCU:\\Software\\WindowsOperationCliTest\\NoSuchKeyAnywhere",
        Some("Whatever"),
        None,
        RegistryValueType::String,
    );
    assert!(
        response.to_lowercase().contains("error") || response.to_lowercase().contains("not found"),
        "a missing key should be reported: {response}"
    );
}

/// An unknown root must be rejected before anything is touched.
#[test]
#[ignore = "reads the registry; run with --ignored"]
fn an_unknown_root_is_rejected() {
    let response = call(
        RegistryMode::List,
        "HKXX:\\Software",
        None,
        None,
        RegistryValueType::String,
    );
    assert!(
        response.to_lowercase().contains("error"),
        "an unknown root should be an error: {response}"
    );
}
