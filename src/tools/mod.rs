//! Tool implementations for the MCP server.
//!
//! Each tool lives in its own submodule (parameter types plus the tool's
//! logic). New tools are added here as they are implemented; the router in
//! `server.rs` wires them up as `#[tool]` methods.

pub mod app;
pub mod caret_info;
pub mod click;
pub mod clipboard;
pub mod cursor_position;
pub mod display_inventory;
pub mod doctor;
pub mod filesystem;
pub mod invoke_element;
pub mod move_mouse;
pub mod multi_edit;
pub mod multi_select;
pub mod notification;
pub mod process;
pub mod record;
pub mod registry;
pub mod scrape;
pub mod screenshot;
pub mod scroll;
pub mod shell;
pub mod shortcut;
pub mod snapshot;
mod support;
pub mod typing;
pub mod wait;
pub mod wait_for;
