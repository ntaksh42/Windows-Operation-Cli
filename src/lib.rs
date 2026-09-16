//! Windows desktop automation MCP server.
//!
//! The crate is a library so integration tests in `tests/` can drive the real
//! tool code paths (`uia`, `state`, `window`, `tools::*`) against a live test
//! window, rather than re-including individual source files with `#[path]` —
//! which only ever worked for modules with no `crate::` references.
//! `src/main.rs` is the thin stdio binary on top of this.

pub mod apps;
pub mod capture;
pub mod display;
pub mod fuzzy;
pub mod ia2;
pub mod input_sim;
pub mod keys;
pub mod overlay;
pub mod params;
pub mod powershell;
pub mod server;
pub mod state;
pub mod tool_policy;
pub mod tools;
pub mod uia;
pub mod vdm;
pub mod win;
pub mod window;
