# Tool parameter reference

Distilled from `src/server.rs` and `docs/SPEC.md`. Parameters marked `?` are optional.

## Contents

- [Observation: Screenshot, Snapshot, DisplayInventory](#observation)
- [Element actions: InvokeElement, Click, Type, Scroll, Move, MultiSelect, MultiEdit](#element-actions)
- [Keyboard: Shortcut](#keyboard)
- [Synchronization: Wait, WaitFor](#synchronization)
- [Apps and windows: App](#apps-and-windows)
- [System: PowerShell, FileSystem, Registry, Process, Clipboard, Notification](#system)
- [Web: Scrape](#web)

## Observation

### Screenshot

Fast capture: text summary + PNG. No UI tree.

| param | type | default | notes |
|---|---|---|---|
| use_annotation | bool | false | currently a no-op — accepted but ignored (unlike Snapshot's) |
| width_reference_line / height_reference_line | int? | null | grid lines; both required to take effect |
| display | [int]? | null | restrict capture to display indices (see DisplayInventory) |

Output text includes cursor position, screenshot size, virtual-desktop and window tables. If the image was downscaled, it reports Original Size and a Coordinate Scale to multiply image coordinates by.

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

## Element actions

### InvokeElement

| param | type | default | notes |
|---|---|---|---|
| element_id | uint64 | required | from the most recent Snapshot only |
| fallback_to_click | bool | false | allow validated coordinate click when no semantic action exists |

Re-resolves the element by RuntimeId within its owning window and executes the first available UIA pattern: Invoke → SelectionItem.Select → Toggle → ExpandCollapse. Errors on stale ids, closed windows, or ambiguous matches.

### Click

| param | type | default | notes |
|---|---|---|---|
| loc | [x,y]? | null | one of loc/label required |
| label | int? | null | element index from latest Snapshot; no staleness check — an old label silently targets whatever now holds that index |
| button | left \| right \| middle | left | |
| clicks | int | 1 | 0=hover, 1=single, 2=double |

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
| mode | read \| write \| copy \| move \| delete \| list \| search \| info | required | |
| path | string | required | relative paths resolve from the user's Desktop |
| destination | string? | | copy/move |
| content | string? | | write |
| pattern | string? | | search (glob, required) / list (optional filter) |
| recursive | bool | false | delete: required for non-empty dirs; list/search: recurse into subdirectories |
| append | bool | false | write |
| overwrite | bool | false | copy/move onto existing target |
| offset / limit | int? | null | read line range; offset is 1-based |
| encoding | string | utf-8 | |
| show_hidden | bool | false | |

read caps at 10 MB. list/search cap at 500 entries. Errors come back as formatted strings ("Error: File not found: ..."), not protocol errors.

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

## Web

### Scrape

| param | type | default | notes |
|---|---|---|---|
| url | string | required | http/https only; private/loopback targets rejected |
| query | string? | null | focus for sampling summarization |
| use_dom | bool | false | read from an already-open browser tab's DOM instead of HTTP |
| use_sampling | bool | true | summarize via MCP sampling when the client supports it |

HTTP mode converts the page to Markdown, follows up to 5 redirects, 10 s timeout. DOM mode requires the URL to be open in a browser first.
