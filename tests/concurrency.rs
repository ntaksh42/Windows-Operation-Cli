//! Calling the tools from several threads at once.
//!
//! The server runs each tool on Tokio's blocking pool, so two calls can be in
//! flight together, and they share one desktop state plus a per-thread COM
//! apartment and cached D3D device. Nothing had exercised that: every test so
//! far calls one tool at a time from one thread.
//!
//! What matters here is that concurrent use is *safe* — no panic, no
//! deadlock, no torn state — not that it is fast. Input injection is
//! inherently serial: two clicks at once are two clicks in some order.

#![cfg(target_os = "windows")]

mod harness;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use harness::{BUTTON_TEXT, TestApp, desktop_lock};

use windows_operation_cli::capture::{Backend, capture_rect_with_backend, virtual_screen_rect};
use windows_operation_cli::params::BoolOrString;
use windows_operation_cli::state;
use windows_operation_cli::tools::snapshot::{SnapshotParams, capture, snapshot};

fn app(title: &str) -> Option<TestApp> {
    let app = TestApp::launch(title)?;
    app.wait_until_on_top(Duration::from_secs(3))?;
    Some(app)
}

/// Several captures at once must all come back with the window's controls.
///
/// Each thread builds its own COM apartment and UIA objects, which is the
/// part that would fail if any of it were shared unsafely.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn concurrent_captures_all_succeed() {
    let _desktop = desktop_lock();
    let Some(app) = app("Concurrent Capture") else {
        return;
    };
    let handle = app.hwnd();

    let succeeded = Arc::new(AtomicU32::new(0));
    let found_button = Arc::new(AtomicU32::new(0));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let succeeded = Arc::clone(&succeeded);
        let found_button = Arc::clone(&found_button);
        threads.push(std::thread::spawn(move || {
            let result = capture(&SnapshotParams {
                use_vision: Some(BoolOrString::Bool(false)),
                timeout_ms: Some(20_000),
                ..Default::default()
            });
            if let Ok(result) = result {
                succeeded.fetch_add(1, Ordering::SeqCst);
                if result
                    .interactive_nodes
                    .iter()
                    .any(|node| node.owner_handle == handle && node.name == BUTTON_TEXT)
                {
                    found_button.fetch_add(1, Ordering::SeqCst);
                }
            }
        }));
    }
    for thread in threads {
        thread.join().expect("a capture thread panicked");
    }

    assert_eq!(
        succeeded.load(Ordering::SeqCst),
        4,
        "not every concurrent capture succeeded"
    );
    assert_eq!(
        found_button.load(Ordering::SeqCst),
        4,
        "a concurrent capture came back without the window's controls"
    );
}

/// Screenshots from several threads share the cached D3D device, and each
/// thread's own copy is released when it exits. That teardown is what used to
/// take the process down.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn concurrent_screenshots_survive_their_threads_exiting() {
    let succeeded = Arc::new(AtomicU32::new(0));
    let mut threads = Vec::new();
    for _ in 0..4 {
        let succeeded = Arc::clone(&succeeded);
        threads.push(std::thread::spawn(move || {
            if capture_rect_with_backend(virtual_screen_rect(), Backend::Auto).is_ok() {
                succeeded.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }
    for thread in threads {
        thread.join().expect("a capture thread did not exit cleanly");
    }
    assert_eq!(
        succeeded.load(Ordering::SeqCst),
        4,
        "not every concurrent screenshot succeeded"
    );
}

/// Two `Snapshot` calls racing to publish desktop state must leave one whole
/// state behind, not a mixture of both.
///
/// A torn state would be the dangerous outcome: ids from one capture resolving
/// against another's nodes, which is precisely what the generation check
/// exists to prevent.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn racing_snapshots_leave_one_consistent_state() {
    let _desktop = desktop_lock();
    let Some(app) = app("Concurrent State") else {
        return;
    };

    let mut threads = Vec::new();
    for _ in 0..4 {
        threads.push(std::thread::spawn(|| {
            let _ = snapshot(&SnapshotParams {
                use_vision: Some(BoolOrString::Bool(false)),
                timeout_ms: Some(20_000),
                ..Default::default()
            });
        }));
    }
    for thread in threads {
        thread.join().expect("a snapshot thread panicked");
    }

    let published = state::current_state().expect("no state was published");
    // Every node has to belong to the generation the state reports, or an id
    // from one capture could resolve against another's nodes.
    for node in published
        .interactive_nodes
        .iter()
        .chain(published.scrollable_nodes.iter())
    {
        assert_eq!(
            node.element_id >> 32,
            published.generation as u64,
            "a node from another generation is in the published state: {:?}",
            node.name
        );
    }
    assert!(
        !published.interactive_nodes.is_empty(),
        "the surviving state has no controls"
    );

    // And the ids it published have to resolve.
    let first = published.interactive_nodes[0].element_id;
    state::resolve_element(first).expect("the published state's own id did not resolve");
    let _ = app;
}

/// A capture running while another thread injects input must not deadlock:
/// both take Windows-level locks, and the capture holds a COM apartment.
#[test]
#[ignore = "requires an interactive Windows desktop session; run with --ignored"]
fn a_capture_and_input_at_once_do_not_deadlock() {
    let _desktop = desktop_lock();
    let Some(app) = app("Concurrent Mixed") else {
        return;
    };

    let start = Instant::now();
    let capture_thread = std::thread::spawn(|| {
        for _ in 0..3 {
            let _ = capture(&SnapshotParams {
                use_vision: Some(BoolOrString::Bool(false)),
                timeout_ms: Some(20_000),
                ..Default::default()
            });
        }
    });
    let input_thread = std::thread::spawn(|| {
        for index in 0..10 {
            let _ = windows_operation_cli::input_sim::set_cursor_pos(300 + index * 5, 300);
            std::thread::sleep(Duration::from_millis(30));
        }
    });

    capture_thread.join().expect("the capture thread panicked");
    input_thread.join().expect("the input thread panicked");
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "capture and input together took {:?}; something is blocking",
        start.elapsed()
    );
    let _ = app;
}
