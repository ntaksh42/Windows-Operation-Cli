---
name: windows-operation-cli
description: Operate the Windows desktop via the windows-operation-cli MCP server. Use whenever a task touches the Windows UI or local system - launching apps, clicking, typing, reading the screen, waiting for windows, managing files/registry/processes/clipboard - even when the user does not name the server.
---

# Windows-Operation-Cli MCP Server

Rust MCP server for Windows desktop automation. Core loop: **observe → act → synchronize**, repeated until the task's goal is verified on screen.

## Core workflow

1. **Observe** the screen:
   - `Screenshot` — fast path: image + cursor position + window summaries. No element ids. Use for visual context and verification. By default it captures the screen as seen; pass `window` (fuzzy title match) to capture just that window even while other windows cover it.
   - `Snapshot` — heavier path: UI accessibility tree with element ids and supported semantic actions. Use when you need to interact with controls. Scans only the foreground app by default; pass `window` (fuzzy title match) to target one app, or `scope="all"` for whole-desktop discovery. `use_vision=true` adds an annotated screenshot.
2. **Act** on elements, preferring semantics over coordinates:
   - `InvokeElement` with an `element_id` from the **most recent** Snapshot — activates the control via UIA patterns (Invoke/Select/Toggle/Expand) without screen coordinates. Most reliable.
   - `Click` / `Type` / `Scroll` / `Move` with `label` (element index from Snapshot) or raw `loc=[x, y]`.
3. **Synchronize** before the next step:
   - Prefer `WaitFor` (polls until a window/text/element appears, returns as soon as the condition is met) over fixed `Wait`, which always burns its full delay.

After acting, re-observe (`Screenshot` is usually enough) to verify the effect before moving on.

## Rules that prevent common failures

- **Element ids and labels are generation-scoped — and fail differently when stale.** A stale `element_id` from an older Snapshot errors out safely ("Element id N is stale"). A stale `label` does not: it silently indexes into the newest Snapshot's element list and may act on a completely different control. Re-read both from the latest Snapshot output; never carry them across Snapshots.
- **Labels require a prior Snapshot.** `Click`/`Type` with `label` fails with "Desktop state is empty" until Snapshot has run at least once.
- **Convert screenshot coordinates before acting on them.** Images may be downscaled (1920x1080 cap x `WINDOWS_MCP_SCREENSHOT_SCALE`), and the capture origin is not always (0, 0). Always apply the formula the output itself reports:
  - `Screenshot Coordinate Scale: S` — multiply: `screen = (image_x × S, image_y × S)`.
  - `Screenshot Coordinate Transform: screen = (origin_x + image_x × S, origin_y + image_y × S)` — apply the offset too. This appears whenever the capture starts at a non-zero desktop origin: a monitor left of or above the primary (where `origin_x`/`origin_y` are **negative** — keep the sign), or a `display`-restricted capture. Multiplying without the offset lands the click on the wrong monitor.
- **UI state changes after every action.** Menus close, dialogs open, focus moves. Do not chain multiple coordinate-based actions from one old observation.
- **`InvokeElement` falls back to clicking only when asked.** Pass `fallback_to_click=true` to allow a validated coordinate click when no semantic action is available; otherwise it errors.
- **Timeout-bound Snapshot.** `timeout_ms` (default 2000, range 100-30000) bounds the UIA scan; on expiry the tree is truncated, not failed. Raise it for large windows, or narrow the scan with `window`.

## Choosing a tool

| Goal | Tool |
|---|---|
| See the screen quickly | `Screenshot` |
| See one window that may be covered | `Screenshot` with `window` (bring it forward with `App` before clicking what you saw) |
| Find and target controls | `Snapshot` (then `InvokeElement`) |
| Launch / focus / resize an app | `App` |
| Run a command, script, or anything non-UI | `PowerShell` |
| Read/write files | `FileSystem` (relative paths resolve from Desktop) |
| Wait for UI state | `WaitFor` (not `Wait`) |
| Keyboard shortcut (copy, alt+tab, win+r) | `Shortcut` |
| Fetch a web page as Markdown | `Scrape` |
| Registry / processes / clipboard / toast | `Registry` / `Process` / `Clipboard` / `Notification` |
| Multi-monitor layout and DPI | `DisplayInventory` |
| Verify caret / selected text after typing | `CaretInfo` |
| Diagnose why automation fails at all (UIA, capture, shell) | `Doctor` |

Prefer non-UI tools when they can do the job: editing a file with `FileSystem` or running `PowerShell` is faster and more reliable than driving Notepad through the UI.

## Typical sequences

Launch an app and use it:

```
App(mode="launch", name="notepad")
WaitFor(condition="active_window", window_name="Notepad")
Snapshot()                          # get element ids
InvokeElement(element_id=...)       # or Type(label=..., text=...)
Screenshot()                        # verify result
```

Fill a form with several fields: `MultiEdit(labels=[[label, text], ...])` instead of repeated Type calls. Select multiple files/items: `MultiSelect`.

`Click` and `Scroll` take a `modifier` (`shift`/`ctrl`/`alt`/`win`) held down for the duration of the action — use it for ctrl+click multi-select or ctrl+wheel zoom instead of composing `Shortcut` with a separate click, which races. `Click` also accepts `clicks=3` for a triple-click (select-line).

Text entry details for `Type`: `clear=true` replaces existing content, `press_enter=true` submits, `caret_position` is `start`/`end`/`idle`. Text of 20+ characters containing none of `\n`, `\t`, `{`, `}` is pasted via clipboard automatically (fast); anything else is sent keystroke by keystroke.

## Full parameter reference

Per-tool parameters, defaults, and response formats: [references/tools.md](references/tools.md). Read it when a call fails validation or you need an exact parameter name or mode.
