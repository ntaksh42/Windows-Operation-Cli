# Tool parameter reference

Distilled from `src/server.rs` and `docs/SPEC.md`. Parameters marked `?` are optional.

## Contents

- [Observation: Screenshot, Snapshot, DisplayInventory, CursorPosition, CaretInfo](#observation)
- [Element actions: InvokeElement, Click, Type, Scroll, Move, MultiSelect, MultiEdit](#element-actions)
- [Keyboard: Shortcut](#keyboard)
- [Synchronization: Wait, WaitFor](#synchronization)
- [Apps and windows: App](#apps-and-windows)
- [System: PowerShell, FileSystem, Registry, Process, Clipboard, Notification, Doctor](#system)
- [Web: Scrape](#web)

## Observation

### Screenshot

Fast capture: text summary + PNG. No UI tree.

| param | type | default | notes |
|---|---|---|---|
| use_annotation | bool | false | currently a no-op — accepted but ignored (unlike Snapshot's) |
| width_reference_line / height_reference_line | int? | null | grid lines; both required to take effect |
| display | [int]? | null | restrict capture to display indices (see DisplayInventory) |
| window | string? | null | fuzzy title match; capture only that window, even when covered. Incompatible with `display` |

Output text includes cursor position, screenshot size, virtual-desktop and window tables, plus the coordinate conversion to apply before passing image pixels to any `loc=` argument:

- `Screenshot Coordinate Scale: S` (image downscaled) — `screen = (image_x × S, image_y × S)`.
- `Screenshot Coordinate Transform: screen = (origin_x + image_x × S, origin_y + image_y × S)` — emitted when the capture origin is non-zero (a monitor left of/above the primary, or a `display`-restricted capture). `origin_x`/`origin_y` may be negative; preserve the sign. Scale alone is wrong here.

Use whichever line the output actually reports.

Screen vs. window capture:

| | screen (default / `display`) | `window` |
|---|---|---|
| Method | GDI `BitBlt` of the composed desktop (DXGI only as fallback) | `PrintWindow(PW_RENDERFULLCONTENT)`: the window renders itself |
| Shows | exactly what is visible, including windows in front | the window alone, even when covered or partly off-screen |
| Use for | deciding where to click; verifying what the user sees | reading a background window without activating it |
| Coordinates | as above (transform only when the origin is non-zero) | always a `Screenshot Coordinate Transform` with origin = window's top-left |
| Fails when | — | window minimized (error: restore it first) |

With `window`, the output has `Screenshot Window: <title>` and `Screenshot Backend: printwindow`. If the window cannot render itself (some GPU-rendered apps), it falls back to the window's screen region and says so in the backend line; windows in front of it then appear in the image. A coordinate taken from a window capture is only clickable if nothing covers that point — bring the window forward (`App`) before clicking.

A `Screenshot Warning:` line means the image came back almost entirely black: Windows gave no desktop content (locked workstation, secure desktop, minimized or disconnected RDP, or a server outside the interactive session). Do not read or click from that image; run `Doctor`.

### Snapshot

UI accessibility tree with element ids and semantic actions.

| param | type | default | notes |
|---|---|---|---|
| scope | "foreground" \| "all" | foreground | `all` = whole-desktop discovery |
| window | string? | null | fuzzy title match for one app; incompatible with scope=all |
| timeout_ms | int | 2000 | 100-30000; on expiry returns partial tree marked truncated |
| use_vision | bool | false | adds annotated screenshot |
| use_dom | bool | false | browser DOM extraction (Chrome/Edge/Firefox); requires use_ui_tree=true |
| use_ui_tree | bool | true | |
| use_annotation | bool | true | number badges on the vision image |
| display | [int]? | null | |
| width_reference_line / height_reference_line | int? | null | |

UI Tree lines look like `(x,y) controltype "name" [action: click]` with element ids. Ids are generation-scoped: a new Snapshot invalidates all previous ids. A stale `element_id` is rejected with an error, but a stale `label` is not detected — it silently indexes into the newest element list (see Click).

### DisplayInventory

No parameters. Returns per-display JSON: index, device, primary, bounds, work_area, resolution, orientation, effective_dpi, scale. The index matches Snapshot/Screenshot's `display` parameter.

### CursorPosition

No parameters. Returns `Cursor position: (x, y)` in screen coordinates — already real screen coordinates, so no scale/transform conversion applies.

### CaretInfo

No parameters. Returns the caret position, or up to 200 characters of selected text, from the focused UI Automation text element. Useful for verifying focus and content after `Type` without a full screenshot.

Errors (not empty results) when the focused element does not support TextPattern, or returns no selection range — treat a failure as "focus is not in a text field", not as a tool fault.

## Element actions

### InvokeElement

| param | type | default | notes |
|---|---|---|---|
| element_id | uint64? | null | from the most recent Snapshot only; one of element_id/element_ids required |
| element_ids | [uint64]? | null | several ids invoked in order, after element_id; stops at the first failure and reports what ran |
| fallback_to_click | bool | false | allow validated coordinate click when no semantic action exists |
| report_text | bool | true | append the text (element names and input-field values) the element's window now shows that it did not show at the last Snapshot |

Re-resolves each element by RuntimeId within its owning window and executes the first available UIA pattern: Invoke → SelectionItem.Select → Toggle → ExpandCollapse. Errors on stale ids, closed windows, or ambiguous matches. With `report_text`, the response ends with `Window text now showing: "72", ...` (up to 15 entries, polled for up to 0.4 s) or `Window text: no change since the last Snapshot.` — enough to verify a result without another Snapshot. Element ids stay valid afterwards.

### Click

| param | type | default | notes |
|---|---|---|---|
| loc | [x,y]? | null | one of loc/label required |
| label | int? | null | element index from latest Snapshot; no staleness check — an old label silently targets whatever now holds that index |
| button | left \| right \| middle | left | |
| clicks | int | 1 | 0=hover, 1=single, 2=double, 3=triple |
| modifier | string? | null | shift / ctrl / alt / win — held down for the whole click, released even on failure |

### Type

| param | type | default | notes |
|---|---|---|---|
| text | string | required | |
| loc / label | | one required | |
| clear | bool | false | Ctrl+A then Backspace first |
| caret_position | start \| idle \| end | idle | |
| press_enter | bool | false | |

Text of 20+ chars containing none of `\n`, `\t`, `{`, `}` is pasted via clipboard (original clipboard restored); otherwise sent per keystroke. `\n` becomes Enter, `\t` becomes Tab.

### Scroll

| param | type | default | notes |
|---|---|---|---|
| loc / label | | optional | omitted = current cursor position |
| type | vertical \| horizontal | vertical | |
| direction | up \| down \| left \| right | down | must match type |
| wheel_times | int | 1 | 1 wheel ≈ 3-5 lines |
| modifier | string? | null | shift / ctrl / alt / win — e.g. ctrl+wheel to zoom |

`type` is the wire name for the axis parameter; send `type`, not `scroll_type`.

### Move

| param | type | default | notes |
|---|---|---|---|
| loc / label | | one required | destination |
| drag | bool | false | mouse-down → move → mouse-up |
| from_loc | [x,y]? | null | drag start; drag=true only (omitted = current position) |
| duration | float? | null | 0-10 s; drag=true only |

Passing from_loc/duration with drag=false is an error.

### MultiSelect

| param | type | default | notes |
|---|---|---|---|
| locs | [[x,y],...]? | | at least one of locs/labels; may combine |
| labels | [int,...]? | | |
| press_ctrl | bool | true | true = Ctrl-click each; false = plain sequential clicks |

### MultiEdit

| param | type | default | notes |
|---|---|---|---|
| locs | [[x,y,text],...]? | | one of locs/labels |
| labels | [[label,text],...]? | | |

Applies Type with clear=true to each field in order.

No target validation happens: each entry is typed at its coordinate whatever is there, and the call reports success even when the target holds no text field. Since `clear=true` sends Ctrl+A then Backspace first, a coordinate that has drifted since the Snapshot wipes whatever *does* have focus. Re-read coordinates from the latest Snapshot, and prefer `labels` over raw `locs`.

## Keyboard

### Shortcut

`shortcut: string` — keys joined by `+`, e.g. `"ctrl+c"`, `"alt+tab"`, `"win+r"`, `"ctrl+shift+esc"`, `"win"`. Aliases: windows/command→win, option→alt, backspace, capslock, scrolllock.

## Synchronization

### Wait

`duration: int` seconds, 1-60. Always sleeps the full duration — prefer WaitFor.

### WaitFor

| param | type | default | notes |
|---|---|---|---|
| condition | string | required | text_exists / active_window / element_exists / element_enabled / focused_element (aliases: text / window / element / enabled / focused) |
| text | string? | | target for text/element conditions (casefold substring) |
| window_name | string? | | target for active_window |
| timeout | float | 10.0 | 0 < timeout ≤ 120 |
| interval | float | 0.25 | 0 < interval ≤ 5 |
| use_dom | bool | false | |

Polls without screenshots; returns as soon as the condition holds. Timeout returns an error result.

## Apps and windows

### App

| param | type | default | notes |
|---|---|---|---|
| mode | launch \| launch_executable \| resize \| switch | launch | |
| name | string? | | launch/resize/switch: fuzzy match (Start Menu name or window title) |
| executable | string? | | launch_executable: full path, must exist |
| args | [string]? | | launch_executable only; separated argv, no shell |
| cwd | string? | | launch_executable only; must be an existing dir |
| window_loc | [x,y]? | | resize |
| window_size | [w,h]? | | resize |
| snapshot | bool | false | launch/resize/switch: append a foreground Snapshot (with element ids) to the response |

Mixing parameters across modes is an error. `launch` resolves the name via Start Menu apps and waits up to 10 s for the window. `launch_executable` returns JSON `{pid, executable, args, cwd}`.

## System

### PowerShell

| param | type | default |
|---|---|---|
| command | string | required |
| timeout | int seconds | 30 |

Response is always `Response: {output}\nStatus Code: {code}`. Runs with -NoProfile, cwd = user home, UTF-8 output. On timeout the process tree is killed and status code 1 returned.

### FileSystem

| param | type | default | notes |
|---|---|---|---|
| mode | read \| write \| edit \| copy \| move \| delete \| list \| search \| info | required | |
| path | string | required | relative paths resolve from the user's Desktop |
| destination | string? | | copy/move |
| content | string? | | write |
| old_text / new_text | string? | | edit; both required together |
| pattern | string? | | search (glob, required) / list (optional filter) |
| content_pattern | string? | | search: regex applied inside the glob-matching files |
| recursive | bool | false | delete: required for non-empty dirs; list/search: recurse into subdirectories |
| append | bool | false | write |
| overwrite | bool | false | copy/move onto existing target |
| dry_run | bool | false | delete: list what would be removed, remove nothing |
| offset / limit | int? | null | read line range; offset is 1-based |
| encoding | string | utf-8 | WHATWG label — utf-8, utf-16le, shift_jis, windows-1252 |
| show_hidden | bool | false | |

read caps at 10 MB. list/search cap at 500 entries. Errors come back as formatted strings ("Error: File not found: ..."), not protocol errors.

**`edit` mode** replaces one exact occurrence, which is what makes it safe to apply blind: `old_text` must match **exactly once**, or nothing is written. Zero matches report the closest line ("Error: old_text was not found in ... Closest line: 4: bravo-two"), several report the count ("matched 2 occurrences ... expected exactly one"). Prefer it over read-modify-`write` for a small change — `write` replaces the whole file, so a stale read silently discards concurrent edits.

**`content_pattern`** turns `search` into grep: `pattern` narrows by filename, `content_pattern` then matches a regex inside those files, and hits come back as `path:line: matched text`. An invalid regex is reported rather than ignored.

**`dry_run`** on delete prints the targets it would remove and removes nothing — worth a first pass whenever `recursive` is involved.

### Registry

| param | type | default | notes |
|---|---|---|---|
| mode | get \| set \| delete \| list | required | |
| path | string | required | PowerShell format: `HKCU:\Software\...`, `HKLM:\...` |
| name | string? | | value name; delete without name removes the whole key |
| value | string? | | set |
| type | string | String | String / ExpandString / Binary / DWord / MultiString / QWord |

set creates missing keys automatically.

### Process

| param | type | default | notes |
|---|---|---|---|
| mode | list \| kill | required | |
| name | string? | | list: fuzzy filter; kill: case-insensitive exact match (may match several) |
| pid | int? | | kill: takes precedence over name |
| sort_by | memory \| cpu \| name | memory | list |
| limit | int | 20 | list |
| force | bool | false | changes response wording only — Windows has no graceful terminate; both paths force-kill |

### Clipboard

`mode: get | set`, `text` required for set. Text only (CF_UNICODETEXT).

### Notification

`title`, `message`, `app_id` — all required. Sends a Windows toast.

### Doctor

No parameters. Returns environment diagnostics as JSON:

```json
{
  "checks": {
    "uia_com": true,            // UI Automation COM init succeeded
    "monitor_count": 2,
    "dxgi": true,               // DXGI screen capture available
    "powershell": "pwsh.exe",   // resolved shell, or null if neither found
    "administrator": false,     // elevated process
    "virtual_desktop_api": true
  },
  "blockers": []                // human-readable strings; empty = healthy
}
```

First thing to run when automation misbehaves for no obvious reason: a non-empty `blockers` array explains why Snapshot, Screenshot, or PowerShell cannot work at all. Always succeeds (never an error result), so the report itself is safe to request.

## Web

### Scrape

| param | type | default | notes |
|---|---|---|---|
| url | string | required | http/https only; private/loopback targets rejected |
| query | string? | null | focus for sampling summarization |
| use_dom | bool | false | read from an already-open browser tab's DOM instead of HTTP |
| use_sampling | bool | true | summarize via MCP sampling when the client supports it |

HTTP mode converts the page to Markdown, follows up to 5 redirects, 10 s timeout. DOM mode requires the URL to be open in a browser first.
