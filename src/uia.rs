//! UIA (UI Automation) COM wrapper used by the `Snapshot`/`WaitFor` tools.
//!
//! Performance-critical: every window's subtree is fetched with a single
//! `FindAllBuildCache` call driven by an `IUIAutomationCacheRequest` that
//! pre-registers the properties/patterns we need. Callers only ever read
//! `Cached*` members — never `Current*` — so there are no per-element
//! cross-process COM round trips (docs/SPEC.md "Snapshot 性能設計").
//!
//! COM must be initialized MTA (`COINIT_MULTITHREADED`) on the calling
//! thread before any function here is used; see [`ensure_com_initialized`].

use std::cell::Cell;
use std::mem::ManuallyDrop;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, POINT, RECT, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{
    VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_ARRAY, VT_BOOL, VT_BSTR, VT_I4, VT_R8,
};
use windows::Win32::UI::Accessibility::*;
use windows::core::{Interface, Result as WinResult};

/// A UI element read from the UIA cache after a `FindAllBuildCache` call.
/// Every field here is a `Cached*` read — no COM round trip per field.
#[derive(Debug, Clone)]
pub struct RawElement {
    pub parent_index: Option<usize>,
    pub runtime_id: Vec<i32>,
    pub supported_actions: Vec<crate::state::SupportedAction>,
    pub control_type: i32,
    pub name: String,
    pub automation_id: String,
    pub rect: RECT,
    pub is_enabled: bool,
    pub is_offscreen: bool,
    pub has_keyboard_focus: bool,
    /// Only meaningful when `control_type == UIA_WindowControlTypeId`.
    pub is_modal: bool,
    /// The provider's own clickable point, when it supplies one. Preferred
    /// over the bounding rectangle's geometric center, which lands outside
    /// the control for non-rectangular shapes and for containers whose
    /// middle is covered by a child.
    pub clickable_point: Option<(i32, i32)>,
    /// Whether `IUIAutomationScrollPattern` is present on this element at all
    /// (presence, not scrollability direction — matches the task's
    /// "ScrollPattern の有無で判定" instruction).
    pub is_scrollable: bool,
    pub vertical_scroll_percent: f64,
    /// `ValuePattern.Value`, cut to [`MAX_VALUE_CHARS`]: what an input field
    /// holds, which its `Name` (the field's label) does not say.
    pub value: String,
}

/// Longest value kept per element. Values are read to report what changed,
/// not to transfer documents.
pub const MAX_VALUE_CHARS: usize = 200;

thread_local! {
    static COM_INITIALIZED: Cell<bool> = const { Cell::new(false) };
}

/// Initializes COM as MTA on the current thread, once. Safe to call
/// repeatedly (including from multiple functions on the same thread).
pub fn ensure_com_initialized() -> Result<(), String> {
    COM_INITIALIZED.with(|initialized| {
        if initialized.get() {
            return Ok(());
        }
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            initialized.set(true);
            Ok(())
        } else {
            Err(format!("CoInitializeEx failed: {hr:?}"))
        }
    })
}

/// COM initialized as MTA for as long as the guard lives, for a short-lived
/// worker thread. [`ensure_com_initialized`] is for threads that live on and
/// never uninitialize; a worker that exits should.
pub struct ComApartment(());

impl ComApartment {
    pub fn enter() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self(()))
        } else {
            Err(format!("CoInitializeEx failed: {hr:?}"))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Creates the `IUIAutomation` root object (`CUIAutomation`).
pub fn create_automation() -> WinResult<IUIAutomation> {
    unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
}

/// Bounds the next UIA provider transaction. `IUIAutomation2` is available on
/// supported Windows versions; callers keep using the base interface for the
/// rest of the API surface.
pub fn set_transaction_timeout(automation: &IUIAutomation, timeout_ms: u32) -> Result<(), String> {
    let automation: IUIAutomation2 = automation.cast().map_err(|error| error.to_string())?;
    unsafe {
        automation
            .SetTransactionTimeout(timeout_ms)
            .map_err(|error| error.to_string())
    }
}

/// Control-type ids considered "interactive" (docs/SPEC.md §6 item 4):
/// Button/CheckBox/ComboBox/Edit/Hyperlink/ListItem/MenuItem/RadioButton/
/// TabItem/TreeItem/SplitButton.
pub const INTERACTIVE_CONTROL_TYPES: &[i32] = &[
    UIA_ButtonControlTypeId.0,
    UIA_CheckBoxControlTypeId.0,
    UIA_ComboBoxControlTypeId.0,
    UIA_EditControlTypeId.0,
    UIA_HyperlinkControlTypeId.0,
    UIA_ListItemControlTypeId.0,
    UIA_MenuItemControlTypeId.0,
    UIA_RadioButtonControlTypeId.0,
    UIA_TabItemControlTypeId.0,
    UIA_TreeItemControlTypeId.0,
    UIA_SplitButtonControlTypeId.0,
];

pub const WINDOW_CONTROL_TYPE: i32 = UIA_WindowControlTypeId.0;
pub const DOCUMENT_CONTROL_TYPE: i32 = UIA_DocumentControlTypeId.0;
pub const TITLE_BAR_CONTROL_TYPE: i32 = UIA_TitleBarControlTypeId.0;

/// Lower-cased display name for a `UIA_*ControlTypeId` value, used to render
/// UI Tree lines (`(x,y) controltype "name" [action: ...]`).
#[allow(non_upper_case_globals)] // matching the windows crate's UIA_*ControlTypeId consts
pub fn control_type_name(control_type: i32) -> String {
    let id = UIA_CONTROLTYPE_ID(control_type);
    let name = match id {
        UIA_ButtonControlTypeId => "button",
        UIA_CalendarControlTypeId => "calendar",
        UIA_CheckBoxControlTypeId => "checkbox",
        UIA_ComboBoxControlTypeId => "combobox",
        UIA_EditControlTypeId => "edit",
        UIA_HyperlinkControlTypeId => "hyperlink",
        UIA_ImageControlTypeId => "image",
        UIA_ListItemControlTypeId => "listitem",
        UIA_ListControlTypeId => "list",
        UIA_MenuBarControlTypeId => "menubar",
        UIA_MenuControlTypeId => "menu",
        UIA_MenuItemControlTypeId => "menuitem",
        UIA_ProgressBarControlTypeId => "progressbar",
        UIA_RadioButtonControlTypeId => "radiobutton",
        UIA_ScrollBarControlTypeId => "scrollbar",
        UIA_SliderControlTypeId => "slider",
        UIA_SpinnerControlTypeId => "spinner",
        UIA_StatusBarControlTypeId => "statusbar",
        UIA_TabControlTypeId => "tab",
        UIA_TabItemControlTypeId => "tabitem",
        UIA_TextControlTypeId => "text",
        UIA_ToolBarControlTypeId => "toolbar",
        UIA_ToolTipControlTypeId => "tooltip",
        UIA_TreeControlTypeId => "tree",
        UIA_TreeItemControlTypeId => "treeitem",
        UIA_CustomControlTypeId => "custom",
        UIA_GroupControlTypeId => "group",
        UIA_ThumbControlTypeId => "thumb",
        UIA_DataGridControlTypeId => "datagrid",
        UIA_DataItemControlTypeId => "dataitem",
        UIA_DocumentControlTypeId => "document",
        UIA_SplitButtonControlTypeId => "splitbutton",
        UIA_WindowControlTypeId => "window",
        UIA_PaneControlTypeId => "pane",
        UIA_HeaderControlTypeId => "header",
        UIA_HeaderItemControlTypeId => "headeritem",
        UIA_TableControlTypeId => "table",
        UIA_TitleBarControlTypeId => "titlebar",
        UIA_SeparatorControlTypeId => "separator",
        UIA_SemanticZoomControlTypeId => "semanticzoom",
        UIA_AppBarControlTypeId => "appbar",
        _ => "control",
    };
    name.to_string()
}

/// Builds a `VARIANT` holding a `VT_I4` value (used for property conditions).
fn variant_i4(value: i32) -> VARIANT {
    let mut variant = VARIANT::default();
    variant.Anonymous.Anonymous = ManuallyDrop::new(VARIANT_0_0 {
        vt: VT_I4,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: VARIANT_0_0_0 { lVal: value },
    });
    variant
}

/// Builds a `VARIANT` holding a `VT_BOOL` value (used for property conditions).
fn variant_bool(value: bool) -> VARIANT {
    let mut variant = VARIANT::default();
    variant.Anonymous.Anonymous = ManuallyDrop::new(VARIANT_0_0 {
        vt: VT_BOOL,
        wReserved1: 0,
        wReserved2: 0,
        wReserved3: 0,
        Anonymous: VARIANT_0_0_0 {
            boolVal: if value { VARIANT_TRUE } else { VARIANT_FALSE },
        },
    });
    variant
}

/// Builds the `IUIAutomationCacheRequest` used for every window walk:
/// registers the properties and patterns docs/SPEC.md §6 item 2 calls out
/// (Name/ControlType/BoundingRectangle/IsEnabled/IsOffscreen/
/// IsKeyboardFocusable/HasKeyboardFocus/AutomationId/ClassName/ClickablePoint,
/// plus Invoke/Value/Toggle/Scroll/SelectionItem/ExpandCollapse/Window pattern
/// availability) so every element read below is a `Cached*` read.
pub fn build_cache_request(
    automation: &IUIAutomation,
    tree_filter: &IUIAutomationCondition,
) -> WinResult<IUIAutomationCacheRequest> {
    let cache = unsafe { automation.CreateCacheRequest()? };
    unsafe {
        // Every read is a cached one, so the elements need no live reference
        // back to their provider. Marshaling one per element cost ~20% of a
        // whole-desktop sweep.
        cache.SetAutomationElementMode(AutomationElementMode_None)?;
        cache.SetTreeScope(TreeScope_Subtree)?;
        cache.SetTreeFilter(tree_filter)?;
        for property in [
            UIA_NamePropertyId,
            UIA_ControlTypePropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsEnabledPropertyId,
            UIA_IsOffscreenPropertyId,
            UIA_IsKeyboardFocusablePropertyId,
            UIA_HasKeyboardFocusPropertyId,
            UIA_AutomationIdPropertyId,
            UIA_ClassNamePropertyId,
            UIA_ClickablePointPropertyId,
            UIA_ValueValuePropertyId,
        ] {
            cache.AddProperty(property)?;
        }
        for pattern in [
            UIA_InvokePatternId,
            UIA_ValuePatternId,
            UIA_TogglePatternId,
            UIA_ScrollPatternId,
            UIA_SelectionItemPatternId,
            UIA_ExpandCollapsePatternId,
            UIA_WindowPatternId,
        ] {
            cache.AddPattern(pattern)?;
        }
    }
    Ok(cache)
}

/// Control types that carry information the caller reads but does not click:
/// a calculator's result, a dialog's message, a status line, a field's label.
///
/// Without these a capture can show every button in a window and none of what
/// the window is telling the user — which is most of what a caller needs in
/// order to decide which button to press.
pub const INFORMATIVE_CONTROL_TYPES: &[i32] = &[
    UIA_TextControlTypeId.0,
    UIA_StatusBarControlTypeId.0,
    UIA_ProgressBarControlTypeId.0,
];

/// Builds the `FindAllBuildCache` filter condition: any of the interactive
/// control types, any element exposing `ScrollPattern`, or a `Window` element
/// (needed to detect nested modal dialogs), plus — when `include_text` — the
/// informative text types. Filtering server-side keeps the marshaled element
/// count down instead of fetching the whole subtree and discarding most of it
/// client-side.
///
/// `include_text` is a real cost, not a preference. Measured across a busy
/// desktop: a foreground capture goes from 106ms to 111ms, but a whole-desktop
/// `scope=all` sweep goes from 2.6s to 4.5s, because text nodes outnumber
/// controls several times over. A caller scanning every window is looking for
/// *which* window to work in; the text inside them is what the follow-up
/// foreground capture is for.
pub fn build_condition(
    automation: &IUIAutomation,
    include_text: bool,
) -> WinResult<IUIAutomationCondition> {
    unsafe {
        let informative: &[i32] = if include_text {
            INFORMATIVE_CONTROL_TYPES
        } else {
            &[]
        };
        let mut conditions: Vec<Option<IUIAutomationCondition>> =
            Vec::with_capacity(INTERACTIVE_CONTROL_TYPES.len() + informative.len() + 2);
        for &control_type in INTERACTIVE_CONTROL_TYPES.iter().chain(informative) {
            conditions.push(Some(automation.CreatePropertyCondition(
                UIA_ControlTypePropertyId,
                &variant_i4(control_type),
            )?));
        }
        conditions.push(Some(automation.CreatePropertyCondition(
            UIA_IsScrollPatternAvailablePropertyId,
            &variant_bool(true),
        )?));
        conditions.push(Some(automation.CreatePropertyCondition(
            UIA_ControlTypePropertyId,
            &variant_i4(WINDOW_CONTROL_TYPE),
        )?));
        automation.CreateOrConditionFromNativeArray(&conditions)
    }
}

/// DOM capture needs informative text nodes as well as actionable controls,
/// so it fetches the complete cached subtree in one cross-process call.
pub fn build_dom_condition(automation: &IUIAutomation) -> WinResult<IUIAutomationCondition> {
    unsafe { automation.CreateTrueCondition() }
}

/// Reads every `Cached*` member of `element` into a [`RawElement`]. Each
/// property read is independently best-effort (defaults on failure) so one
/// missing property doesn't drop the whole element.
unsafe fn read_element(element: &IUIAutomationElement) -> RawElement {
    unsafe {
        let control_type = element.CachedControlType().map(|c| c.0).unwrap_or_default();
        let name = element
            .CachedName()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let automation_id = element
            .CachedAutomationId()
            .map(|b| b.to_string())
            .unwrap_or_default();
        let rect = element.CachedBoundingRectangle().unwrap_or_default();
        let is_enabled = element
            .CachedIsEnabled()
            .map(|b| b.as_bool())
            .unwrap_or(false);
        let is_offscreen = element
            .CachedIsOffscreen()
            .map(|b| b.as_bool())
            .unwrap_or(true);
        let has_keyboard_focus = element
            .CachedHasKeyboardFocus()
            .map(|b| b.as_bool())
            .unwrap_or(false);

        let is_modal = if control_type == WINDOW_CONTROL_TYPE {
            element
                .GetCachedPatternAs::<IUIAutomationWindowPattern>(UIA_WindowPatternId)
                .ok()
                .and_then(|pattern| pattern.CachedIsModal().ok())
                .map(|b| b.as_bool())
                .unwrap_or(false)
        } else {
            false
        };

        let scroll_pattern = element
            .GetCachedPatternAs::<IUIAutomationScrollPattern>(UIA_ScrollPatternId)
            .ok();
        let is_scrollable = scroll_pattern.is_some();
        let vertical_scroll_percent = scroll_pattern
            .and_then(|pattern| pattern.CachedVerticalScrollPercent().ok())
            .filter(|percent| percent.is_finite() && *percent >= 0.0)
            .unwrap_or(0.0);

        let runtime_id = element
            .GetRuntimeId()
            .ok()
            .and_then(|array| runtime_id_from_safe_array(array).ok())
            .unwrap_or_default();
        let mut supported_actions = Vec::new();
        if element
            .GetCachedPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
            .is_ok()
        {
            supported_actions.push(crate::state::SupportedAction::Invoke);
        }
        if element
            .GetCachedPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId)
            .is_ok()
        {
            supported_actions.push(crate::state::SupportedAction::SelectionItem);
        }
        if element
            .GetCachedPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)
            .is_ok()
        {
            supported_actions.push(crate::state::SupportedAction::Toggle);
        }
        if element
            .GetCachedPatternAs::<IUIAutomationExpandCollapsePattern>(UIA_ExpandCollapsePatternId)
            .is_ok()
        {
            supported_actions.push(crate::state::SupportedAction::ExpandCollapse);
        }

        // The clickable point is read from the cache rather than through
        // `GetClickablePoint`, a live cross-process call measured at ~12ms per
        // element — most of a foreground capture's cost, for a value most
        // providers do not even supply. It is still only used where the
        // geometric center is in doubt (see `needs_clickable_point`).
        let clickable_point = if needs_clickable_point(control_type, &rect) {
            cached_clickable_point(element)
        } else {
            None
        };

        RawElement {
            parent_index: None,
            runtime_id,
            supported_actions,
            control_type,
            name,
            automation_id,
            rect,
            is_enabled,
            is_offscreen,
            has_keyboard_focus,
            is_modal,
            is_scrollable,
            vertical_scroll_percent,
            clickable_point,
            value: cached_value(element),
        }
    }
}

/// Whether an element's geometric center is unreliable enough to prefer the
/// provider's clickable point over it.
///
/// Containers and large controls are the cases that miss in practice: a tab
/// item, list item, or menu item whose middle is covered by a child, and
/// wide/tall controls whose center falls in padding. Small leaf controls
/// (an ordinary button) are hit correctly by their center, so they skip it.
#[allow(non_upper_case_globals)] // matching the windows crate's UIA_*ControlTypeId consts
fn needs_clickable_point(control_type: i32, rect: &RECT) -> bool {
    const LARGE_EDGE: i32 = 200;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        return false;
    }
    let is_container_type = matches!(
        UIA_CONTROLTYPE_ID(control_type),
        UIA_ListItemControlTypeId
            | UIA_MenuItemControlTypeId
            | UIA_TabItemControlTypeId
            | UIA_TreeItemControlTypeId
            | UIA_ComboBoxControlTypeId
            | UIA_SplitButtonControlTypeId
    );
    is_container_type || width >= LARGE_EDGE || height >= LARGE_EDGE
}

/// Reads the cached `ValuePattern.Value`, or an empty string when the
/// element has none.
unsafe fn cached_value(element: &IUIAutomationElement) -> String {
    unsafe {
        let Ok(value) = element.GetCachedPropertyValue(UIA_ValueValuePropertyId) else {
            return String::new();
        };
        let raw = &value.Anonymous.Anonymous;
        if raw.vt != VT_BSTR {
            return String::new();
        }
        raw.Anonymous
            .bstrVal
            .to_string()
            .chars()
            .take(MAX_VALUE_CHARS)
            .collect()
    }
}

/// Reads the cached `ClickablePoint` property: a `VT_R8` array of `[x, y]`,
/// or empty when the provider supplies none.
unsafe fn cached_clickable_point(element: &IUIAutomationElement) -> Option<(i32, i32)> {
    unsafe {
        let value = element
            .GetCachedPropertyValue(UIA_ClickablePointPropertyId)
            .ok()?;
        let raw = &value.Anonymous.Anonymous;
        if raw.vt != VT_ARRAY | VT_R8 {
            return None;
        }
        let array = raw.Anonymous.parray;
        if array.is_null()
            || SafeArrayGetUBound(array, 1).ok()? - SafeArrayGetLBound(array, 1).ok()? < 1
        {
            return None;
        }
        let mut data = std::ptr::null_mut();
        SafeArrayAccessData(array, &mut data).ok()?;
        let point = std::slice::from_raw_parts(data.cast::<f64>(), 2);
        let point = (point[0].round() as i32, point[1].round() as i32);
        let _ = SafeArrayUnaccessData(array);
        Some(point)
    }
}

unsafe fn runtime_id_from_safe_array(
    array: *mut windows::Win32::System::Com::SAFEARRAY,
) -> WinResult<Vec<i32>> {
    unsafe {
        if array.is_null() {
            return Ok(Vec::new());
        }
        let result = (|| {
            let lower = SafeArrayGetLBound(array, 1)?;
            let upper = SafeArrayGetUBound(array, 1)?;
            if upper < lower {
                return Ok(Vec::new());
            }
            let mut data = std::ptr::null_mut();
            SafeArrayAccessData(array, &mut data)?;
            let values =
                std::slice::from_raw_parts(data.cast::<i32>(), (upper - lower + 1) as usize)
                    .to_vec();
            SafeArrayUnaccessData(array)?;
            Ok(values)
        })();
        let _ = SafeArrayDestroy(array);
        result
    }
}

/// Walks one top-level window's subtree in a single `FindAllBuildCache`
/// call, returning the window's own (root) element plus every matching
/// descendant, in document order.
///
/// This is the hot path the CacheRequest design exists for: one subtree
/// cache build per window, followed only by `Cached*` reads. A filtered
/// `FindAllBuildCache` call is retained as a compatibility fallback.
pub fn walk_window(
    automation: &IUIAutomation,
    cache_request: &IUIAutomationCacheRequest,
    condition: &IUIAutomationCondition,
    hwnd: HWND,
) -> WinResult<(RawElement, Vec<RawElement>)> {
    unsafe {
        let root = automation.ElementFromHandleBuildCache(hwnd, cache_request)?;
        let root_raw = read_element(&root);
        let mut elements = Vec::new();
        let walked = collect_cached_children(&root, None, &mut elements);

        // The cached walk descends through `GetCachedChildren`, which only
        // returns children matching the cache request's TreeFilter. A window
        // whose root's immediate children are all filtered out — Task Manager
        // puts its controls under panes the filter rejects — yields an empty
        // *successful* walk, and the tree is reported as having no controls
        // at all. Fall back whenever the walk produced nothing, not only when
        // it errored: `FindAllBuildCache` searches the whole subtree rather
        // than stopping at the first non-matching level.
        if walked.is_err() || elements.is_empty() {
            elements.clear();
            // The cached root holds no live reference (`AutomationElementMode_None`),
            // so searching from it needs one fetched fresh.
            let live_root = automation.ElementFromHandle(hwnd)?;
            let array = live_root.FindAllBuildCache(TreeScope_Subtree, condition, cache_request)?;
            let len = array.Length()?.max(0) as usize;
            elements.reserve(len);
            for i in 0..len as i32 {
                let element = array.GetElement(i)?;
                elements.push(read_element(&element));
            }
        }
        Ok((root_raw, elements))
    }
}

fn find_matching_element(
    automation: &IUIAutomation,
    identity: &crate::state::ElementNode,
) -> Result<IUIAutomationElement, String> {
    let hwnd = HWND(identity.owner_handle as *mut _);
    unsafe {
        let root = automation
            .ElementFromHandle(hwnd)
            .map_err(|_| "Element owner window is closed".to_string())?;
        // AutomationId is stable for the common case. Probe that small subset
        // first; retain the full RuntimeId search below when it cannot prove a
        // unique match, preserving the stricter identity contract.
        if !identity.automation_id.is_empty() {
            let automation_id = VARIANT::from(identity.automation_id.as_str());
            if let Ok(condition) =
                automation.CreatePropertyCondition(UIA_AutomationIdPropertyId, &automation_id)
                && let Ok(candidates) = root.FindAll(TreeScope_Subtree, &condition)
            {
                let mut exact_matches = Vec::new();
                for index in 0..candidates.Length().map_err(|error| error.to_string())? {
                    let element = candidates
                        .GetElement(index)
                        .map_err(|error| error.to_string())?;
                    let runtime_id = element.GetRuntimeId().map_err(|error| error.to_string())?;
                    let current = runtime_id_from_safe_array(runtime_id)
                        .map_err(|error| error.to_string())?;
                    if current == identity.runtime_id {
                        exact_matches.push(element);
                    }
                }
                match exact_matches.len() {
                    1 => return Ok(exact_matches.remove(0)),
                    count if count > 1 => {
                        return Err(format!(
                            "Element {} runtime identity matched {count} elements",
                            identity.element_id
                        ));
                    }
                    _ => {}
                }
            }
        }
        let condition = automation
            .CreateTrueCondition()
            .map_err(|error| error.to_string())?;
        let array = root
            .FindAll(TreeScope_Subtree, &condition)
            .map_err(|error| error.to_string())?;
        let mut matches = Vec::new();
        let mut guarded_fallbacks = Vec::new();
        for index in 0..array.Length().map_err(|error| error.to_string())? {
            let element = array.GetElement(index).map_err(|error| error.to_string())?;
            let runtime_id = element.GetRuntimeId().map_err(|error| error.to_string())?;
            let current =
                runtime_id_from_safe_array(runtime_id).map_err(|error| error.to_string())?;
            if current == identity.runtime_id {
                matches.push(element);
                continue;
            }
            if !identity.automation_id.is_empty()
                && element
                    .CurrentAutomationId()
                    .is_ok_and(|value| value == identity.automation_id.as_str())
                && element
                    .CurrentControlType()
                    .is_ok_and(|value| control_type_name(value.0) == identity.control_type)
                && element.CurrentBoundingRectangle().is_ok_and(|rect| {
                    (rect.left, rect.top, rect.right, rect.bottom) == identity.bounding_box
                })
            {
                guarded_fallbacks.push(element);
            }
        }
        let matches = if matches.is_empty() {
            guarded_fallbacks
        } else {
            matches
        };
        if matches.len() != 1 {
            return Err(format!(
                "Element {} runtime identity matched {} elements",
                identity.element_id,
                matches.len()
            ));
        }
        Ok(matches[0].clone())
    }
}

pub fn invoke_matching_element(
    identity: &crate::state::ElementNode,
) -> Result<crate::state::SupportedAction, String> {
    if identity.runtime_id.is_empty() {
        return Err(format!(
            "Element {} has no runtime identity",
            identity.element_id
        ));
    }
    ensure_com_initialized()?;
    let automation = create_automation().map_err(|error| error.to_string())?;
    let mut element = find_matching_element(&automation, identity)?;
    if unsafe { element.CurrentIsOffscreen() }
        .map_err(|error| error.to_string())?
        .as_bool()
    {
        let scroll_item = unsafe {
            element.GetCurrentPatternAs::<IUIAutomationScrollItemPattern>(UIA_ScrollItemPatternId)
        }
        .map_err(|_| {
            format!(
                "Element {} is offscreen and does not support ScrollItemPattern",
                identity.element_id
            )
        })?;
        unsafe { scroll_item.ScrollIntoView() }.map_err(|error| error.to_string())?;
        std::thread::sleep(Duration::from_millis(100));
        element = find_matching_element(&automation, identity)?;
    }
    let action = crate::state::SupportedAction::highest_priority(&identity.supported_actions)
        .ok_or_else(|| {
            format!(
                "Element {} has no supported semantic UIA action",
                identity.element_id
            )
        })?;
    unsafe {
        let result: WinResult<()> = (|| {
            match action {
                crate::state::SupportedAction::Invoke => element
                    .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
                    .Invoke()?,
                crate::state::SupportedAction::SelectionItem => element
                    .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                        UIA_SelectionItemPatternId,
                    )?
                    .Select()?,
                crate::state::SupportedAction::Toggle => element
                    .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?
                    .Toggle()?,
                crate::state::SupportedAction::ExpandCollapse => {
                    let pattern = element
                        .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                            UIA_ExpandCollapsePatternId,
                        )?;
                    if pattern.CurrentExpandCollapseState()? == ExpandCollapseState_Collapsed {
                        pattern.Expand()?;
                    } else {
                        pattern.Collapse()?;
                    }
                }
            }
            Ok(())
        })();
        result.map_err(|error| error.to_string())?;
    }
    Ok(action)
}

/// What [`identify_point`] found at a screen coordinate, relative to the
/// element a Snapshot recorded there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointIdentity {
    /// The provider reports the recorded element (or one of its descendants)
    /// at this point — the click will land on what the caller asked for.
    Matches,
    /// Something else is there now. The layout moved under the saved
    /// coordinates.
    Differs,
    /// The provider could not be asked. Callers treat this as "no evidence
    /// either way" and proceed, rather than blocking a click on a UIA hiccup.
    Unknown,
}

/// Checks whether `identity`'s element is still the thing at its saved center.
///
/// Snapshot's generation counter only invalidates ids when a *new* capture is
/// taken; a list that scrolls or a pane that relayouts in between leaves the
/// ids valid and the coordinates stale, so a click lands on whatever moved
/// into that spot. `ElementFromPoint` is one cross-process call that settles
/// it: if the runtime id at the point no longer matches, the element moved.
///
/// A hit on a *descendant* counts as a match. Providers commonly report the
/// innermost element at a point — the text inside a button rather than the
/// button — and that click still reaches the recorded control.
pub fn identify_point(identity: &crate::state::ElementNode, x: i32, y: i32) -> PointIdentity {
    if identity.runtime_id.is_empty() {
        return PointIdentity::Unknown;
    }
    if ensure_com_initialized().is_err() {
        return PointIdentity::Unknown;
    }
    let Ok(automation) = create_automation() else {
        return PointIdentity::Unknown;
    };
    unsafe {
        let Ok(hit) = automation.ElementFromPoint(POINT { x, y }) else {
            return PointIdentity::Unknown;
        };
        let Ok(walker) = automation.RawViewWalker() else {
            return PointIdentity::Unknown;
        };
        // Walk up from the hit element: a match at any ancestor means the
        // point is inside the recorded element.
        let mut current = hit;
        for _ in 0..ANCESTOR_PROBE_LIMIT {
            let Ok(runtime_id) = current.GetRuntimeId() else {
                return PointIdentity::Unknown;
            };
            match runtime_id_from_safe_array(runtime_id) {
                Ok(id) if id == identity.runtime_id => return PointIdentity::Matches,
                Ok(_) => {}
                Err(_) => return PointIdentity::Unknown,
            }
            match walker.GetParentElement(&current) {
                Ok(parent) => current = parent,
                Err(_) => break,
            }
        }
        PointIdentity::Differs
    }
}

/// How far up the ancestor chain [`identify_point`] looks for the recorded
/// element. Deep enough for the wrapper nesting web content produces, bounded
/// so a pathological tree cannot stall a click.
const ANCESTOR_PROBE_LIMIT: usize = 12;

/// Clears the focused element's text through `ValuePattern::SetValue`.
///
/// The keyboard route (Ctrl+A, Backspace) relies on the focused control
/// implementing select-all itself. A bare Win32 `EDIT` does not — in a dialog
/// the dialog manager translates Ctrl+A — so the chord selects nothing and the
/// Backspace deletes a single character, leaving the old text spliced onto the
/// new one. Asking the provider to set an empty value has no such dependency.
///
/// Returns `Ok(false)` when the focused element exposes no writable
/// `ValuePattern`, so the caller can fall back to the keyboard route for
/// controls this cannot serve (a rich-text document, a custom canvas).
pub fn clear_focused_element_value() -> Result<bool, String> {
    ensure_com_initialized()?;
    let automation = create_automation().map_err(|error| error.to_string())?;
    unsafe {
        let Ok(element) = automation.GetFocusedElement() else {
            return Ok(false);
        };
        let Ok(pattern) =
            element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
        else {
            return Ok(false);
        };
        if pattern
            .CurrentIsReadOnly()
            .map(|read_only| read_only.as_bool())
            .unwrap_or(true)
        {
            return Ok(false);
        }
        match pattern.SetValue(&windows::core::BSTR::from("")) {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

/// Reads the focused element's text through `ValuePattern`, when it exposes
/// one.
///
/// Used to confirm a clipboard paste actually landed: the paste is a key
/// chord whose effect the sender cannot otherwise observe, and the clipboard
/// gets restored moments later, so a target that read it late produced a
/// silent partial failure. `None` means "cannot tell" — no focused element, or
/// one with no `ValuePattern` (a rich-text document, a custom canvas) — which
/// callers treat as inconclusive rather than as failure.
pub fn focused_element_value() -> Option<String> {
    ensure_com_initialized().ok()?;
    let automation = create_automation().ok()?;
    unsafe {
        let element = automation.GetFocusedElement().ok()?;
        let pattern = element
            .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
            .ok()?;
        pattern.CurrentValue().ok().map(|value| value.to_string())
    }
}

pub fn caret_info() -> Result<String, String> {
    ensure_com_initialized()?;
    let automation = create_automation().map_err(|error| error.to_string())?;
    unsafe {
        let element = automation
            .GetFocusedElement()
            .map_err(|error| format!("Failed to get focused element: {error}"))?;
        let name = element
            .CurrentName()
            .map(|name| name.to_string())
            .unwrap_or_default();
        let pattern = element
            .GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
            .map_err(|_| "Focused element does not support TextPattern".to_string())?;
        let selections = pattern
            .GetSelection()
            .map_err(|error| format!("Failed to get text selection: {error}"))?;
        if selections.Length().map_err(|error| error.to_string())? == 0 {
            return Err("Focused text element returned no selection range".to_string());
        }
        let range = selections
            .GetElement(0)
            .map_err(|error| error.to_string())?;
        let collapsed = range
            .CompareEndpoints(
                TextPatternRangeEndpoint_Start,
                &range,
                TextPatternRangeEndpoint_End,
            )
            .map_err(|error| error.to_string())?
            == 0;
        if collapsed {
            let document = pattern.DocumentRange().map_err(|error| error.to_string())?;
            document
                .MoveEndpointByRange(
                    TextPatternRangeEndpoint_End,
                    &range,
                    TextPatternRangeEndpoint_Start,
                )
                .map_err(|error| error.to_string())?;
            let prefix = document
                .GetText(-1)
                .map_err(|error| error.to_string())?
                .to_string();
            return Ok(format!(
                "Caret position: {} in {:?}",
                prefix.chars().count(),
                name
            ));
        }

        let text = range
            .GetText(200)
            .map_err(|error| error.to_string())?
            .to_string();
        Ok(format!(
            "Selected text: {}",
            text.chars().take(200).collect::<String>()
        ))
    }
}

unsafe fn collect_cached_children(
    element: &IUIAutomationElement,
    parent_index: Option<usize>,
    output: &mut Vec<RawElement>,
) -> WinResult<()> {
    unsafe {
        // A leaf answers with S_OK and a null array, which windows-rs surfaces
        // as an error carrying no HRESULT. Treating that as a failure aborted
        // the walk at the first leaf, so every window paid for a second,
        // uncached `FindAllBuildCache` pass on top of this one.
        let array = match element.GetCachedChildren() {
            Ok(array) => array,
            Err(error) if error.code().is_ok() => return Ok(()),
            Err(error) => return Err(error),
        };
        let len = array.Length()?.max(0);
        for index in 0..len {
            let child = array.GetElement(index)?;
            let child_index = output.len();
            let mut raw = read_element(&child);
            raw.parent_index = parent_index;
            output.push(raw);
            collect_cached_children(&child, Some(child_index), output)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod clickable_point_tests {
    use super::*;

    fn rect(width: i32, height: i32) -> RECT {
        RECT {
            left: 10,
            top: 20,
            right: 10 + width,
            bottom: 20 + height,
        }
    }

    #[test]
    fn container_types_always_ask_the_provider() {
        // These nest children over their own midpoint, so the geometric
        // center is unreliable no matter how small the control is.
        for control_type in [
            UIA_ListItemControlTypeId,
            UIA_MenuItemControlTypeId,
            UIA_TabItemControlTypeId,
            UIA_TreeItemControlTypeId,
            UIA_ComboBoxControlTypeId,
            UIA_SplitButtonControlTypeId,
        ] {
            assert!(
                needs_clickable_point(control_type.0, &rect(40, 20)),
                "{control_type:?} should ask for a clickable point"
            );
        }
    }

    #[test]
    fn a_small_plain_button_uses_its_center() {
        // The common case: skipping the cross-process call here is what keeps
        // the cached walk cheap.
        assert!(!needs_clickable_point(UIA_ButtonControlTypeId.0, &rect(80, 24)));
    }

    #[test]
    fn a_large_control_asks_even_when_it_is_not_a_container() {
        // Wide or tall controls put their center in padding or over a child.
        assert!(needs_clickable_point(UIA_ButtonControlTypeId.0, &rect(200, 24)));
        assert!(needs_clickable_point(UIA_ButtonControlTypeId.0, &rect(80, 200)));
        assert!(!needs_clickable_point(UIA_ButtonControlTypeId.0, &rect(199, 199)));
    }

    #[test]
    fn a_degenerate_rect_is_never_probed() {
        assert!(!needs_clickable_point(UIA_ListItemControlTypeId.0, &rect(0, 20)));
        assert!(!needs_clickable_point(UIA_ButtonControlTypeId.0, &rect(300, 0)));
    }
}
