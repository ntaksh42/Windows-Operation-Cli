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
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayDestroy, SafeArrayGetLBound, SafeArrayGetUBound,
    SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{VARIANT, VARIANT_0_0, VARIANT_0_0_0, VT_BOOL, VT_I4};
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
}

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
/// IsKeyboardFocusable/HasKeyboardFocus/AutomationId/ClassName, plus
/// Invoke/Value/Toggle/Scroll/SelectionItem/ExpandCollapse/Window pattern
/// availability) so every element read below is a `Cached*` read.
pub fn build_cache_request(
    automation: &IUIAutomation,
    tree_filter: &IUIAutomationCondition,
) -> WinResult<IUIAutomationCacheRequest> {
    let cache = unsafe { automation.CreateCacheRequest()? };
    unsafe {
        cache.SetAutomationElementMode(AutomationElementMode_Full)?;
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

/// Builds the `FindAllBuildCache` filter condition: any of the interactive
/// control types, any element exposing `ScrollPattern`, or a `Window`
/// element (needed to detect nested modal dialogs). Filtering server-side
/// keeps the marshaled element count down instead of fetching the whole
/// subtree and discarding most of it client-side.
pub fn build_condition(automation: &IUIAutomation) -> WinResult<IUIAutomationCondition> {
    unsafe {
        let mut conditions: Vec<Option<IUIAutomationCondition>> =
            Vec::with_capacity(INTERACTIVE_CONTROL_TYPES.len() + 2);
        for &control_type in INTERACTIVE_CONTROL_TYPES {
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

        // GetClickablePoint is a live cross-process call, not a Cached* read,
        // so it is asked only for elements whose geometric center is actually
        // in doubt (see `needs_clickable_point`). Everything else keeps the
        // one-cache-build-per-window cost model.
        let clickable_point = if needs_clickable_point(control_type, &rect) {
            let mut point = POINT::default();
            match element.GetClickablePoint(&mut point) {
                Ok(got) if got.as_bool() => Some((point.x, point.y)),
                _ => None,
            }
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
        }
    }
}

/// Whether an element's geometric center is unreliable enough to justify the
/// extra cross-process `GetClickablePoint` call.
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
    reverse_children: bool,
) -> WinResult<(RawElement, Vec<RawElement>)> {
    unsafe {
        let root = automation.ElementFromHandleBuildCache(hwnd, cache_request)?;
        let root_raw = read_element(&root);
        let mut elements = Vec::new();
        if collect_cached_children(&root, None, reverse_children, &mut elements).is_err() {
            let array = root.FindAllBuildCache(TreeScope_Subtree, condition, cache_request)?;
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
    reverse_children: bool,
    output: &mut Vec<RawElement>,
) -> WinResult<()> {
    unsafe {
        let array = element.GetCachedChildren()?;
        let len = array.Length()?.max(0);
        let indices: Box<dyn Iterator<Item = i32>> = if reverse_children {
            Box::new((0..len).rev())
        } else {
            Box::new(0..len)
        };
        for index in indices {
            let child = array.GetElement(index)?;
            let child_index = output.len();
            let mut raw = read_element(&child);
            raw.parent_index = parent_index;
            output.push(raw);
            collect_cached_children(&child, Some(child_index), reverse_children, output)?;
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
