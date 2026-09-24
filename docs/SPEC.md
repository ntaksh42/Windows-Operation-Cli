# Windows-Operation-Cli — ツール仕様書（Python 版 Windows-MCP から抽出）

Rust 再実装の基準仕様。元実装: E:\Windows-MCP\src\windows_mcp\

## 全体方針（Rust 版での適用）

- MCP クライアント（特に Claude Desktop）は配列や bool を JSON 文字列で送ることがある。パラメータは `bool` / `Vec<i32>` に加え、JSON 文字列フォールバックを受け付けること（Python 版の `_as_bool` / `_as_loc` 相当）。
- 期待されるエラー（ファイル未存在など）は整形済み文字列で返す。想定外の例外は MCP の isError として返す。
- Rust 版では PostHog テレメトリは実装しない（自分用のため不要）。
- fuzzy マッチ: App/ウィンドウ名は score_cutoff 70、Process 名フィルタは partial_ratio > 60。Rust では rapidfuzz 系クレートで同等に。

---

## 1. App

modes: launch / launch_executable / resize / switch

| param | type | default |
|---|---|---|
| mode | enum | "launch" |
| name | string? | null (launch/resize/switch 用。Start Menu に fuzzy マッチ) |
| window_loc | [x,y]? | null (resize) |
| window_size | [w,h]? | null (resize) |
| executable | string? | null (launch_executable 必須。存在チェック、expanduser+resolve) |
| args | [string]? | null (launch_executable のみ) |
| cwd | string? | null (launch_executable のみ。存在する dir 必須) |
| snapshot | bool | false (launch/resize/switch。対象ウィンドウの Snapshot を応答に追記し、要素 id を発行する) |

- モード外パラメータ混在は ValueError。
- launch_executable: `Popen([exe,*args], shell=False, stdio=null)` 相当 → JSON `{"pid","executable","args","cwd"}` を返す。
- launch: Get-StartApps (PowerShell) で列挙 → fuzzy(70) → パスなら Start-Process、AppID なら ShellExecute で `shell:AppsFolder\<AppID>`（失敗時は "Invalid app identifier"）。一覧はプロセス内にキャッシュし（サーバー起動時にバックグラウンドで取得、App 無効時は取得しない）、名前が見つからない時だけ取り直す。起動後 PID→ウィンドウ出現を最大 10 秒待つ。起動前から存在したウィンドウは、1.5 秒経っても新しいウィンドウが現れない場合のみ採用する。snapshot=true では要素数が 2 回続けて同じになるまで（最大 3 秒）撮り直す。応答: `"{Name} launched."` / `"Launching {Name} sent, but window not detected yet."` / `"{name} not found in start menu."`
- resize: MoveWindow。minimized/maximized なら拒否文字列。応答 `"{name} resized to {w}x{h} at {x},{y}."`
- switch: AttachThreadInput + SetForegroundWindow + BringWindowToTop + SetWindowPos。応答 `"Switched to {Name} window."` 等。

## 2. DisplayInventory

パラメータなし。ディスプレイごとに JSON: index, device, primary, bounds{}, work_area{}, resolution "WxH", orientation, effective_dpi, scale。
EnumDisplayMonitors + GetMonitorInfoExW + GetDpiForMonitor(Shcore)。index は ATTACHED_TO_DESKTOP のみ数えた 0 始まり（Snapshot の display パラメータと同じ空間）。

## 3. PowerShell

| param | type | default |
|---|---|---|
| command | string | 必須 |
| timeout | int 秒 | 30 |

応答は常に `"Response: {output}\nStatus Code: {code}"`。

実装要点:
- pwsh 優先、なければ powershell 5.1。
- シェルは UTF-8 出力設定プレフィックス付きのブートストラップを `-NoProfile -EncodedCommand`（UTF-16LE → base64、5.1 のみ `-OutputFormat Text` 追加）で起動し、コマンドは stdin から base64(UTF-8) で受け取って `-Command` 相当（script scope に dot-source、最終文失敗で exit 1、構文エラーは無出力で exit 1）で実行する。
- 起動（~500ms）を呼び出しの外に出すため、各呼び出しの後に次回用のシェルを 1 つ事前起動しておく。コマンドごとに新しいプロセスである点は変わらない。起動時とシェル・環境変数が異なれば破棄して起動し直す。
- stdin はコマンド送信後に閉じる（MCP stdio パイプは継承しない）。cwd はユーザーホーム固定。NO_COLOR=1。サーバーにコンソールが無い場合は CREATE_NO_WINDOW。
- 環境変数修復: HKLM/HKCU の Environment から欠損分を補完（PATH は継承優先で追記・大小無視デデュープ、PATHEXT フォールバックあり）。
- タイムアウト: CTRL_BREAK_EVENT → 2 秒猶予 → `taskkill /PID x /T /F` でツリーごと強制終了。応答 `("Command execution timed out", 1)`。
- returncode≠0 かつ "Access is denied" かつ非管理者なら昇格ヒントを追記。

## 4. FileSystem

modes: read / write / copy / move / delete / list / search / info

| param | type | default |
|---|---|---|
| mode | enum | 必須 |
| path | string | 必須（相対パスは**デスクトップ基準**で解決） |
| destination | string? | copy/move 必須 |
| content | string? | write 必須 |
| pattern | string? | search 必須 / list 任意 |
| recursive | bool | false |
| append | bool | false |
| overwrite | bool | false |
| offset / limit | int? | null（read の行範囲、offset は 1 始まり） |
| encoding | string | "utf-8" |
| show_hidden | bool | false |

- read: 10 MB 上限。`"File: {path}\n{content}"` / 行範囲時 `"Lines {s}-{e} of {total}:"`。エラーは `"Error: File not found: ..."` 等の整形文字列（raise しない）。
- write: 親 dir 自動作成。`"Written to {path} ({size:,} bytes)"` / append 時 `"Appended to ..."`。
- copy: copy2/copytree。dest 存在 && !overwrite はエラー文字列。
- delete: dir 非空は recursive 必須。
- list/search: `[DIR ]`/`[FILE]` 行、dirs-first ソート、500 件で truncate。サイズは B/KB/MB/GB (1 桁小数)。
- info: Path/Type/Size/Created/Modified/Accessed/Read-only + dir なら Contents、file なら Extension。
- PermissionError には非管理者時ヒント追記。

## 5. Registry

modes: get / set / delete / list。PowerShell cmdlet 経由（Get-ItemProperty 等）。ps_quote で単一引用符エスケープ。
- path は PowerShell 形式 (`HKCU:\Software\...`)。
- set: type は String/ExpandString/Binary/DWord/MultiString/QWord。キーなければ New-Item -Force。応答 `'Registry value [{path}] "{name}" set to "{value}" (type: {type}).'`
- get: `'Registry value [{path}] "{name}" = {value}'`
- delete: name 省略時キー全削除。
- list: Values (Format-List) + Sub-Keys。

Rust 版では windows crate の Registry API 直呼びでも可（応答文字列フォーマットは維持）。

## 6. Snapshot

UIA 走査は既定でフォアグラウンドアプリのみ（同一PIDのポップアップを含む）。`scope` は
`foreground`（既定）/`all`、`window` は単一アプリの代表ウィンドウのタイトル曖昧検索。
`window` と `scope=all` は併用不可。`timeout_ms` は UIA 走査全体の期限で、
既定 2000、範囲 100..=30000。期限切れ時は取得済み要素を返し、UI Tree に
truncated 表示を含める。

| param | type | default |
|---|---|---|
| use_vision | bool | false |
| use_dom | bool | false（use_ui_tree=true 必須） |
| use_annotation | bool | true |
| use_ui_tree | bool | true |
| width_reference_line / height_reference_line | int? | null（グリッド線描画、両方指定で有効） |
| display | [int]? | null（アクティブディスプレイ index の配列、キャプチャ領域を union に制限） |

応答: [テキスト] または [テキスト, PNG 画像]。テキスト構造:
```
Cursor Position: (x, y)
Screenshot Size: (W,H)  ← 縮小時は Original Size + Coordinate Scale (逆数を掛けよの指示)
Visible Displays: ...
[Screenshot Backend: ...]  ← use_vision 時

Active Desktop: / All Desktops: （仮想デスクトップ名テーブル）
Focused Window: / Opened Windows: （Name Depth Status Width Height Handle テーブル）

UI Tree:
desktop
├── window "..."
│   ├── (x,y) controltype "name" [action: click]
```
- 1920x1080 キャップ + WINDOWS_MCP_SCREENSHOT_SCALE (0.1–1.0) を乗算、LANCZOS 縮小。
- interactive_nodes / scrollable_nodes のフラットリストをサーバー状態に保持 → Click 等の label 解決に使う（label は interactive 先、超えた分は scrollable のオフセット）。Snapshot 未実行での label 使用は "Desktop state is empty. Please call Snapshot first."
- 各 interactive 要素は Snapshot 世代を含む `element_id`、`parent_id`、所有 HWND、
  RuntimeId、AutomationId、ControlType、bounds、対応 semantic actions を保持する。
  新しい Snapshot は前世代の element_id を無効化する。
- 注釈画像: interactive_nodes の index を番号としてバウンディングボックス描画。
- ブラウザ (chrome/msedge/firefox) + use_dom: Chromium は UIA の RootWebArea、Firefox は IA2 フォールバック。
- モーダルダイアログ検出時はそのウィンドウの蓄積ノードをクリア。
- ウィンドウ列挙: EnumWindows + Progman + Shell_TrayWnd、現仮想デスクトップのみ、オーバーレイ除外。
- 子要素の走査順は文書順（ラベル番号の順序に影響）。
- エラー時: `"Error capturing desktop state: {e}. Please try again."`（文字列で返す）

## 7. Screenshot

Snapshot の固定版: use_vision=true, use_ui_tree=false, use_dom=false。params: use_annotation(default false), width/height_reference_line, display。
UI Tree セクションの代わりに固定文: "UI Tree: Skipped for fast screenshot-only capture. Call Snapshot when you need interactive or scrollable elements."
応答は [テキスト, PNG]。キャップ・スケール処理は Snapshot と同一。

## 8. Click

| param | type | default |
|---|---|---|
| loc | [x,y]? | null |
| label | int? | null |
| button | left/right/middle | left |
| clicks | int | 1（0=hover, 2=double） |

loc/label どちらか必須。応答 `"{Hover|Single|Double} {button} clicked at ({x},{y})."`

## 8.1 InvokeElement

`element_id: uint64?`、`element_ids: [uint64]?`（少なくとも一方。複数は順に実行し、失敗時はそこで止めて実行済みを報告）、
`fallback_to_click: bool=false`、`report_text: bool=true`（実行後、要素のウィンドウに最後の Snapshot 以降新たに現れた
テキスト（要素名と ValuePattern の値。値は 200 文字まで）を最大 15 件、最大 0.4 秒ポーリングして報告。要素 id は無効化しない）。最新 Snapshot の
構造化要素を所有ウィンドウ内で RuntimeId により一意に再解決し、Invoke、
SelectionItem.Select、Toggle、ExpandCollapse の順で利用可能なUIAパターンを
実行する。RuntimeId一致がない場合のみ、AutomationId、ControlType、boundsの
全一致を再解決に使用する。古いID、閉じたウィンドウ、複数一致はエラー。
座標クリックは明示指定時だけ許可し、保存された中心が現在の所有ウィンドウと
要素bounds内にあることを検証する。
Rust では SendInput（MOUSEEVENTF_ABSOLUTE は仮想スクリーン基準に正規化）。double click は GetDoubleClickTime()/2 の間隔。click 間 sleep 0.05s、操作後は既定 0.02s（`WINDOWS_MCP_INPUT_SETTLE_MS` で変更可能）。

## 9. Type

| param | type | default |
|---|---|---|
| text | string | 必須 |
| loc / label | | どちらか必須 |
| clear | bool | false（Ctrl+A → Backspace） |
| caret_position | start/idle/end | idle（Home/End キー） |
| press_enter | bool | false |

応答 `"Typed {text} at ({x},{y})."`
手順: クリックでフォーカス → caret → clear → 入力。**20 文字以上かつ制御文字なし（\n \t { } を含まない）ならクリップボード経由ペースト**（元内容を保存→復元）。それ以外は 1 文字ずつ送信 (interval 0.04s)。\n→Enter、\t→Tab。press_enter で最後に Enter。

## 10. Scroll

| param | type | default |
|---|---|---|
| loc / label | | 任意（省略時カーソル位置） |
| type | horizontal/vertical | vertical |
| direction | up/down/left/right | down |
| wheel_times | int | 1 |

WHEEL_DELTA=120 単位、notch 間 0.05s。horizontal は Shift 押下 + 縦ホイールで代替。
応答 `"Scrolled {type} {direction} by {wheel_times} wheel times at ({x},{y})."` 不正組合せはエラー文字列を返す（raise しない）。

## 11. Move

| param | type | default |
|---|---|---|
| loc / label | | どちらか必須 |
| drag | bool | false |
| from_loc | [x,y]? | null（drag=true 時のみ。省略時は現カーソル位置から） |
| duration | float? | null（0–10 秒。drag=true 時のみ） |

- move: SetCursorPos で即時移動。応答 `"Moved the mouse pointer to ({x},{y})."`
- drag: mouse-down → 移動（duration 指定時は 0.01s 刻み最大 200 ステップ線形補間）→ mouse-up。例外時も mouse-up を保証。応答 `"Dragged from ({sx},{sy}) to ({x},{y})[ over {d:.3f} seconds]."`
- from_loc/duration を drag=false で渡すと ValueError。

## 12. Shortcut

param: shortcut: string（例 "ctrl+shift+esc"）。`+` 分割 → エイリアス（backspace→Back, capslock→Capital, scrolllock→Scroll, windows/command→Win, option→Alt）→ 同時押しチョードとして送信。応答 `"Pressed {shortcut}."`

## 13. Wait

param: duration: int 秒。sleep。応答 `"Waited for {duration} seconds."`
（Rust 版スキャフォールドでは 1–60 範囲チェック追加済み — 維持してよい）

## 14. WaitFor

| param | type | default |
|---|---|---|
| condition | string | 必須: text_exists / active_window / element_exists / element_enabled / focused_element（別名 text/window/element/enabled/focused、`-`→`_`、小文字化） |
| text | string? | 条件による必須 |
| window_name | string? | |
| timeout | float | 10.0（0<t≤120） |
| interval | float | 0.25（0<i≤5） |
| use_dom | bool | false |

内部で Snapshot 相当（vision なし）をポーリング。マッチは casefold 部分文字列。focused_element は has_focused メタデータ必須。
成功: `"WaitFor condition '{cond}' satisfied after {elapsed:.2f}s and {n} attempt(s): {detail}."`
タイムアウト: TimeoutError を raise（isError になる）。

## 15. Scrape

| param | type | default |
|---|---|---|
| url | string | 必須 |
| query | string? | null |
| use_dom | bool | false |
| use_sampling | bool | true |

- use_dom=false: 生 HTTP GET（リダイレクト手動 5 hop まで、各 hop で SSRF 検証: http/https のみ・認証情報 URL 拒否・解決先 IP が private/loopback/link-local 等なら拒否）→ HTML→Markdown 変換。timeout 10s。
- use_dom=true: ブラウザの DOM スナップショットからテキスト集約 + スクロール位置バナー。未検出時 `"No DOM information found. Please open {url} in browser first."`
- use_sampling: MCP サンプリング (ctx.sample) で要約。クライアント非対応なら黙って raw 返し。
- 応答: `"URL: {url}\nContent:\n{content}"`

## 16. MultiSelect

locs: [[x,y],...] / labels: [int,...]（併用可、少なくとも一方）、press_ctrl: bool=true。
Ctrl 押しっぱなし → 各座標クリック（操作後は `WINDOWS_MCP_INPUT_SETTLE_MS`、既定 0.02s）→ Ctrl 解放（無条件）。
応答 `"Multi-selected elements at:\n(x,y)\n(x,y)..."`

## 17. MultiEdit

locs: [[x,y,text],...] / labels: [[label,text],...]。各要素に Type(clear=true) を順次適用。
応答 `"Multi-edited elements at: (x,y) with text 'text', ..."`

## 18. Clipboard

mode: get/set、text（set 必須）。CF_UNICODETEXT。
- get: `"Clipboard content:\n{data}"` / `"Clipboard is empty or contains non-text data."`
- set: `"Clipboard set to: {text 先頭100文字}[...]"` / text なし `"Error: text parameter required for set mode."`

## 19. Process

| param | type | default |
|---|---|---|
| mode | list/kill | 必須 |
| name | string? | list: fuzzy(partial>60) フィルタ / kill: 大小無視**完全一致**（複数可） |
| pid | int? | kill 優先ターゲット |
| sort_by | memory/cpu/name | memory |
| limit | int | 20 |
| force | bool | false（kill vs terminate） |

- list: `"Processes ({n} shown):\n"` + PID/Name/CPU%/Memory テーブル（"12.3%", "456.7 MB"）。
- kill: `"{Force killed|Terminated}: {name} (PID {pid})"` / `"No process with PID {pid} found."` / access denied ヒント等。

## 20. Notification

title, message, app_id（全部必須）。WinRT ToastNotificationManager（ToastGeneric テンプレート、text 2 行）。
応答 `'Notification sent: "{title}" - {message}'`。失敗しても文字列で返す。
Rust では windows crate の WinRT (Windows.UI.Notifications) 直呼びで可。

---

## 環境変数（Rust 版でも維持）

- `WINDOWS_MCP_SCREENSHOT_SCALE`: 0.1–1.0、default 1.0
- `WINDOWS_MCP_SCREENSHOT_BACKEND`: auto/dxcam/mss/pillow → Rust では auto/dxgi/gdi に読み替え
- `WINDOWS_MCP_PROFILE_SNAPSHOT`: ステージ別タイミングログ
- テレメトリ関連 (ANONYMIZED_TELEMETRY 等) は不要

## Snapshot 性能設計（Rust 版の本丸）

- UIA IUIAutomation を COM 直呼び。**CacheRequest に必要プロパティ（Name, ControlType, BoundingRectangle, IsEnabled, IsOffscreen, IsKeyboardFocusable, HasKeyboardFocus, AutomationId, ClassName 等）とパターン（Invoke/Value/Toggle/Scroll/SelectionItem/ExpandCollapse）を登録し、FindAll(TreeScope_Subtree or Children, cached) で一括取得**。要素ごとの cross-process 往復を排除する。
- 通常操作は foreground または明示 window のみ走査し、全ウィンドウ走査は
  `scope=all` の発見用途に限定する。全ウィンドウの背景要素はリトライせず、
  すべての走査は共有 deadline を超えない。
- `WINDOWS_MCP_PROFILE_SNAPSHOT=1` で window enumeration、UIA、image、total の
  各時間をstderrへ出力する。4K画像処理はrelease profileで測定・運用する。
- UI操作後の同期には固定秒数のWaitよりWaitForを優先する。
- 走査対象ウィンドウは最大 8 スレッドで並列に走査する（各スレッドが自前の MTA と UIA オブジェクトを持つ）。結果は走査対象の順に後処理する。
- CacheRequest は `AutomationElementMode_None`（読むのはキャッシュ値のみ）。Value もキャッシュし、200 文字に切り詰めて保持する。ClickablePoint もキャッシュから読み、要素ごとの `GetClickablePoint` 往復はしない。
- リトライ: COM 一時失敗に指数バックオフ（Python 版 THREAD_MAX_RETRIES=3）。
