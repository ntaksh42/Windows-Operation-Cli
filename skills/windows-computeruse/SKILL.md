---
name: windows-computeruse
description: Operate the Windows desktop via the windows-computeruse MCP server. Use whenever a task touches the Windows UI or local system - launching apps, clicking, typing, reading the screen, waiting for windows, managing files/registry/processes/clipboard - even when the user does not name the server.
---

# Windows-ComputerUse MCP Server

Rust MCP server for Windows desktop automation. Core loop: **observe → act → synchronize**, repeated until the task's goal is verified on screen.

## Core workflow

1. **Observe** the screen:
   - `Screenshot` — fast path: image + cursor position + window summaries. No element ids. Use for visual context and verification.
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
- **Scale screenshot coordinates.** Screenshots may be downscaled (1920x1080 cap x `WINDOWS_MCP_SCREENSHOT_SCALE`). When the output reports an Original Size / Coordinate Scale, multiply image coordinates by the stated ratio before passing them to Click/Move/Type.
- **UI state changes after every action.** Menus close, dialogs open, focus moves. Do not chain multiple coordinate-based actions from one old observation.
- **`InvokeElement` falls back to clicking only when asked.** Pass `fallback_to_click=true` to allow a validated coordinate click when no semantic action is available; otherwise it errors.
- **Timeout-bound Snapshot.** `timeout_ms` (default 2000, range 100-30000) bounds the UIA scan; on expiry the tree is truncated, not failed. Raise it for large windows, or narrow the scan with `window`.

## Choosing a tool

| Goal | Tool |
|---|---|
| See the screen quickly | `Screenshot` |
| Find and target controls | `Snapshot` (then `InvokeElement`) |
| Launch / focus / resize an app | `App` |
| Run a command, script, or anything non-UI | `PowerShell` |
| Read/write files | `FileSystem` (relative paths resolve from Desktop) |
| Wait for UI state | `WaitFor` (not `Wait`) |
| Keyboard shortcut (copy, alt+tab, win+r) | `Shortcut` |
| Fetch a web page as Markdown | `Scrape` |
| Registry / processes / clipboard / toast | `Registry` / `Process` / `Clipboard` / `Notification` |
| Multi-monitor layout and DPI | `DisplayInventory` |

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

Text entry details for `Type`: `clear=true` replaces existing content, `press_enter=true` submits, `caret_position` is `start`/`end`/`idle`. Text of 20+ characters containing none of `\n`, `\t`, `{`, `}` is pasted via clipboard automatically (fast); anything else is sent keystroke by keystroke.

## Full parameter reference

Per-tool parameters, defaults, and response formats: [references/tools.md](references/tools.md). Read it when a call fails validation or you need an exact parameter name or mode.
