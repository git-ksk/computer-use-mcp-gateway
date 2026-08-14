//! Cua browser observation normalization for the V2 semantic surface.
//!
//! Raw target ids, tab ids, page refs, snapshot ids, and continuations remain
//! internal. This module validates the provider shape and converts only bounded
//! backend-neutral fields. Public CUMG refs are minted separately.

use crate::v2_browser::BrowserAction;
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;

pub const MAX_BROWSER_TABS: usize = 128;
pub const MAX_BROWSER_SNAPSHOT_REFS: usize = 2_048;
pub const MAX_BROWSER_OUTLINE_BYTES: usize = 512 * 1024;
pub const MAX_BROWSER_ROLE_BYTES: usize = 128;
pub const MAX_BROWSER_NAME_BYTES: usize = 8 * 1024;
pub const MAX_BROWSER_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_BROWSER_STATE_VALUES: usize = 32;
pub const MAX_BROWSER_STATE_BYTES: usize = 256;
pub const MAX_BROWSER_FRAME_BYTES: usize = 128;
pub const MAX_BROWSER_VISIBILITY_BYTES: usize = 128;
pub const MAX_BROWSER_BACKEND_HANDLE_BYTES: usize = 4 * 1024;
pub const MAX_BROWSER_STRUCTURED_METADATA_BYTES: usize = 2 * 1024 * 1024;

fn enforce_structured_metadata_bound(value: &Value) -> Result<(), BrowserNormalizeError> {
    let bytes = serde_json::to_vec(value).map_err(|_| BrowserNormalizeError::InvalidShape)?;
    if bytes.len() > MAX_BROWSER_STRUCTURED_METADATA_BYTES {
        return Err(BrowserNormalizeError::ValueTooLarge);
    }
    Ok(())
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NormalizedCuaBrowserBinding {
    backend_target_id: String,
    tabs: Vec<NormalizedCuaBrowserTab>,
}

impl NormalizedCuaBrowserBinding {
    pub(crate) fn backend_target_id(&self) -> &str {
        &self.backend_target_id
    }

    pub(crate) fn tabs(&self) -> &[NormalizedCuaBrowserTab] {
        &self.tabs
    }
}

impl fmt::Debug for NormalizedCuaBrowserBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NormalizedCuaBrowserBinding")
            .field("backend_target_id", &"[redacted]")
            .field("tabs", &self.tabs)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NormalizedCuaBrowserTab {
    backend_tab_id: String,
    pub(crate) title: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) active: Option<bool>,
}

impl NormalizedCuaBrowserTab {
    pub(crate) fn backend_tab_id(&self) -> &str {
        &self.backend_tab_id
    }
}

impl fmt::Debug for NormalizedCuaBrowserTab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NormalizedCuaBrowserTab")
            .field("backend_tab_id", &"[redacted]")
            .field("title", &self.title)
            .field("url", &self.url.as_ref().map(|_| "[redacted page url]"))
            .field("active", &self.active)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NormalizedCuaBrowserSnapshot {
    backend_snapshot_id: String,
    pub(crate) outline: String,
    pub(crate) action_refs: Vec<NormalizedCuaBrowserRef>,
    pub(crate) content_refs: Vec<NormalizedCuaBrowserRef>,
    pub(crate) complete: bool,
    pub(crate) omitted: u32,
    backend_continuation: Option<String>,
    pub(crate) screenshot_width: Option<u32>,
    pub(crate) screenshot_height: Option<u32>,
    pub(crate) viewport_css_width: Option<u32>,
    pub(crate) viewport_css_height: Option<u32>,
}

impl NormalizedCuaBrowserSnapshot {
    pub(crate) fn backend_snapshot_id(&self) -> &str {
        &self.backend_snapshot_id
    }

    pub(crate) fn backend_continuation(&self) -> Option<&str> {
        self.backend_continuation.as_deref()
    }
}

impl fmt::Debug for NormalizedCuaBrowserSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NormalizedCuaBrowserSnapshot")
            .field("backend_snapshot_id", &"[redacted]")
            .field("outline_bytes", &self.outline.len())
            .field("action_refs", &self.action_refs)
            .field("content_refs", &self.content_refs)
            .field("complete", &self.complete)
            .field("omitted", &self.omitted)
            .field(
                "backend_continuation",
                &self.backend_continuation.as_ref().map(|_| "[redacted]"),
            )
            .field("screenshot_width", &self.screenshot_width)
            .field("screenshot_height", &self.screenshot_height)
            .field("viewport_css_width", &self.viewport_css_width)
            .field("viewport_css_height", &self.viewport_css_height)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct NormalizedCuaBrowserRef {
    backend_ref: String,
    pub(crate) role: String,
    pub(crate) name: Option<String>,
    pub(crate) value: Option<String>,
    pub(crate) states: Vec<String>,
    pub(crate) actions: Vec<BrowserAction>,
    pub(crate) frame: String,
    pub(crate) visibility: String,
}

impl NormalizedCuaBrowserRef {
    pub(crate) fn backend_ref(&self) -> &str {
        &self.backend_ref
    }
}

impl fmt::Debug for NormalizedCuaBrowserRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NormalizedCuaBrowserRef")
            .field("backend_ref", &"[redacted]")
            .field("role", &self.role)
            .field("name", &self.name)
            .field("value", &self.value.as_ref().map(|_| "[redacted value]"))
            .field("states", &self.states)
            .field("actions", &self.actions)
            .field("frame", &self.frame)
            .field("visibility", &self.visibility)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserNormalizeError {
    InvalidShape,
    BackendRefused,
    NonExactBinding,
    MutationNotAllowed,
    TooManyTabs,
    TooManyRefs,
    ValueTooLarge,
    InvalidBackendHandle,
    InvalidActionRef,
    InvalidScreenshotMetadata,
    BackendIdentityMismatch,
}

impl fmt::Display for BrowserNormalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for BrowserNormalizeError {}

pub(crate) fn normalize_cua_browser_binding(
    value: &Value,
) -> Result<NormalizedCuaBrowserBinding, BrowserNormalizeError> {
    require_ok(value)?;
    enforce_structured_metadata_bound(value)?;
    if string(value, "mode")? != "bind" {
        return Err(BrowserNormalizeError::InvalidShape);
    }
    if string(value, "binding_quality")? != "exact" {
        return Err(BrowserNormalizeError::NonExactBinding);
    }
    if value.get("mutation_allowed").and_then(Value::as_bool) != Some(true) {
        return Err(BrowserNormalizeError::MutationNotAllowed);
    }
    let backend_target_id = backend_handle(string(value, "target_id")?)?;
    let raw_tabs = value
        .get("tabs")
        .and_then(Value::as_array)
        .ok_or(BrowserNormalizeError::InvalidShape)?;
    if raw_tabs.len() > MAX_BROWSER_TABS {
        return Err(BrowserNormalizeError::TooManyTabs);
    }
    let mut tabs = Vec::with_capacity(raw_tabs.len());
    let mut seen = HashSet::with_capacity(raw_tabs.len());
    for raw in raw_tabs {
        let backend_tab_id = backend_handle(string(raw, "tab_id")?)?;
        if !seen.insert(backend_tab_id.clone()) {
            return Err(BrowserNormalizeError::InvalidShape);
        }
        tabs.push(NormalizedCuaBrowserTab {
            backend_tab_id,
            title: optional_bounded_string(raw, "title", MAX_BROWSER_NAME_BYTES)?,
            url: optional_bounded_string(raw, "url", MAX_BROWSER_VALUE_BYTES)?,
            active: optional_bool(raw, "active")?,
        });
    }
    Ok(NormalizedCuaBrowserBinding {
        backend_target_id,
        tabs,
    })
}

pub(crate) fn normalize_cua_browser_snapshot(
    value: &Value,
    expected_backend_target: &str,
    expected_backend_tab: &str,
) -> Result<NormalizedCuaBrowserSnapshot, BrowserNormalizeError> {
    require_ok(value)?;
    enforce_structured_metadata_bound(value)?;
    if string(value, "mode")? != "snapshot" {
        return Err(BrowserNormalizeError::InvalidShape);
    }
    if string(value, "target_id")? != expected_backend_target
        || string(value, "tab_id")? != expected_backend_tab
    {
        return Err(BrowserNormalizeError::BackendIdentityMismatch);
    }
    let snapshot = value
        .get("snapshot")
        .and_then(Value::as_object)
        .ok_or(BrowserNormalizeError::InvalidShape)?;
    if snapshot.get("format").and_then(Value::as_str) != Some("semantic_v2") {
        return Err(BrowserNormalizeError::InvalidShape);
    }
    let backend_snapshot_id = backend_handle(
        snapshot
            .get("id")
            .and_then(Value::as_str)
            .ok_or(BrowserNormalizeError::InvalidShape)?,
    )?;
    let complete = snapshot
        .get("complete")
        .and_then(Value::as_bool)
        .ok_or(BrowserNormalizeError::InvalidShape)?;
    let backend_continuation = match snapshot.get("continuation") {
        None | Some(Value::Null) => None,
        Some(Value::String(value)) => Some(backend_handle(value)?),
        Some(_) => return Err(BrowserNormalizeError::InvalidShape),
    };
    let omitted = sum_omissions(snapshot.get("omitted"))?;
    let outline = bounded_multiline_string(value, "outline", MAX_BROWSER_OUTLINE_BYTES)?;
    let action_refs = normalize_ref_array(value.get("refs"), true)?;
    let content_refs = normalize_ref_array(value.get("content_refs"), false)?;
    if action_refs.len().saturating_add(content_refs.len()) > MAX_BROWSER_SNAPSHOT_REFS {
        return Err(BrowserNormalizeError::TooManyRefs);
    }
    let screenshot = normalize_screenshot_metadata(value)?;
    Ok(NormalizedCuaBrowserSnapshot {
        backend_snapshot_id,
        outline,
        action_refs,
        content_refs,
        complete,
        omitted,
        backend_continuation,
        screenshot_width: screenshot.map(|item| item.0),
        screenshot_height: screenshot.map(|item| item.1),
        viewport_css_width: screenshot.map(|item| item.2),
        viewport_css_height: screenshot.map(|item| item.3),
    })
}

fn normalize_ref_array(
    value: Option<&Value>,
    action_authority: bool,
) -> Result<Vec<NormalizedCuaBrowserRef>, BrowserNormalizeError> {
    let raw = value
        .and_then(Value::as_array)
        .ok_or(BrowserNormalizeError::InvalidShape)?;
    if raw.len() > MAX_BROWSER_SNAPSHOT_REFS {
        return Err(BrowserNormalizeError::TooManyRefs);
    }
    let mut refs = Vec::with_capacity(raw.len());
    let mut seen = HashSet::with_capacity(raw.len());
    for item in raw {
        let backend_ref = backend_handle(string(item, "ref")?)?;
        if !seen.insert(backend_ref.clone()) {
            return Err(BrowserNormalizeError::InvalidShape);
        }
        let actions = if action_authority {
            normalize_actions(item.get("actions"))?
        } else {
            Vec::new()
        };
        if action_authority && actions.is_empty() {
            return Err(BrowserNormalizeError::InvalidActionRef);
        }
        refs.push(NormalizedCuaBrowserRef {
            backend_ref,
            role: bounded_string(item, "role", MAX_BROWSER_ROLE_BYTES)?,
            name: optional_bounded_string(item, "name", MAX_BROWSER_NAME_BYTES)?,
            value: optional_bounded_string(item, "value", MAX_BROWSER_VALUE_BYTES)?,
            states: normalize_states(item.get("states"))?,
            actions,
            frame: bounded_string(item, "frame", MAX_BROWSER_FRAME_BYTES)?,
            visibility: bounded_string(item, "visibility", MAX_BROWSER_VISIBILITY_BYTES)?,
        });
    }
    Ok(refs)
}

fn normalize_actions(value: Option<&Value>) -> Result<Vec<BrowserAction>, BrowserNormalizeError> {
    let raw = value
        .and_then(Value::as_array)
        .ok_or(BrowserNormalizeError::InvalidShape)?;
    let mut actions = Vec::new();
    for item in raw {
        let Some(action) = item.as_str() else {
            return Err(BrowserNormalizeError::InvalidShape);
        };
        let action = match action {
            "click" => Some(BrowserAction::Click),
            "type" => Some(BrowserAction::Type),
            "pointer" => Some(BrowserAction::Pointer),
            "scroll" => Some(BrowserAction::Scroll),
            "upload" => Some(BrowserAction::Upload),
            _ => None,
        };
        if let Some(action) = action {
            if !actions.contains(&action) {
                actions.push(action);
            }
        }
    }
    Ok(actions)
}

fn normalize_screenshot_metadata(
    value: &Value,
) -> Result<Option<(u32, u32, u32, u32)>, BrowserNormalizeError> {
    let Some(screenshot) = value.get("screenshot") else {
        return Ok(None);
    };
    let screenshot = screenshot
        .as_object()
        .ok_or(BrowserNormalizeError::InvalidScreenshotMetadata)?;
    if screenshot.get("mime_type").and_then(Value::as_str) != Some("image/png")
        || screenshot.get("coordinate_space").and_then(Value::as_str) != Some("viewport_css_px")
    {
        return Err(BrowserNormalizeError::InvalidScreenshotMetadata);
    }
    let width = positive_u32(screenshot.get("width"))?;
    let height = positive_u32(screenshot.get("height"))?;
    let css_width = positive_u32(screenshot.get("viewport_css_width"))?;
    let css_height = positive_u32(screenshot.get("viewport_css_height"))?;
    Ok(Some((width, height, css_width, css_height)))
}

fn sum_omissions(value: Option<&Value>) -> Result<u32, BrowserNormalizeError> {
    let Some(object) = value.and_then(Value::as_object) else {
        return Err(BrowserNormalizeError::InvalidShape);
    };
    let mut total = 0_u32;
    for value in object.values() {
        let raw = value.as_u64().ok_or(BrowserNormalizeError::InvalidShape)?;
        let count = u32::try_from(raw).map_err(|_| BrowserNormalizeError::ValueTooLarge)?;
        total = total
            .checked_add(count)
            .ok_or(BrowserNormalizeError::ValueTooLarge)?;
    }
    Ok(total)
}

fn positive_u32(value: Option<&Value>) -> Result<u32, BrowserNormalizeError> {
    let value = value.ok_or(BrowserNormalizeError::InvalidScreenshotMetadata)?;
    let raw = if let Some(raw) = value.as_u64() {
        raw
    } else {
        let raw = value
            .as_f64()
            .filter(|raw| raw.is_finite() && *raw > 0.0 && raw.fract() == 0.0)
            .ok_or(BrowserNormalizeError::InvalidScreenshotMetadata)?;
        if raw > u32::MAX as f64 {
            return Err(BrowserNormalizeError::InvalidScreenshotMetadata);
        }
        raw as u64
    };
    let value = u32::try_from(raw).map_err(|_| BrowserNormalizeError::InvalidScreenshotMetadata)?;
    if value == 0 {
        return Err(BrowserNormalizeError::InvalidScreenshotMetadata);
    }
    Ok(value)
}

fn require_ok(value: &Value) -> Result<(), BrowserNormalizeError> {
    match value.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(()),
        Some("refused") => Err(BrowserNormalizeError::BackendRefused),
        _ => Err(BrowserNormalizeError::InvalidShape),
    }
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, BrowserNormalizeError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(BrowserNormalizeError::InvalidShape)
}

fn bounded_string(value: &Value, key: &str, limit: usize) -> Result<String, BrowserNormalizeError> {
    let value = string(value, key)?;
    if value.len() > limit || has_disallowed_control(value, false) {
        return Err(BrowserNormalizeError::ValueTooLarge);
    }
    Ok(value.to_owned())
}

fn bounded_multiline_string(
    value: &Value,
    key: &str,
    limit: usize,
) -> Result<String, BrowserNormalizeError> {
    let value = string(value, key)?;
    if value.len() > limit || has_disallowed_control(value, true) {
        return Err(BrowserNormalizeError::ValueTooLarge);
    }
    Ok(value.to_owned())
}

fn optional_bounded_string(
    value: &Value,
    key: &str,
    limit: usize,
) -> Result<Option<String>, BrowserNormalizeError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            if value.len() > limit || has_disallowed_control(value, false) {
                return Err(BrowserNormalizeError::ValueTooLarge);
            }
            Ok(Some(value.clone()))
        }
        Some(_) => Err(BrowserNormalizeError::InvalidShape),
    }
}

fn optional_bool(value: &Value, key: &str) -> Result<Option<bool>, BrowserNormalizeError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(BrowserNormalizeError::InvalidShape),
    }
}

fn normalize_states(value: Option<&Value>) -> Result<Vec<String>, BrowserNormalizeError> {
    const KNOWN: &[&str] = &[
        "checked",
        "disabled",
        "editable",
        "expanded",
        "focused",
        "focusable",
        "pressed",
        "required",
        "selected",
    ];
    let raw = value
        .and_then(Value::as_object)
        .ok_or(BrowserNormalizeError::InvalidShape)?;
    if raw.len() > MAX_BROWSER_STATE_VALUES {
        return Err(BrowserNormalizeError::ValueTooLarge);
    }
    let mut states = Vec::new();
    for (key, value) in raw {
        if !KNOWN.contains(&key.as_str()) || key.len() > MAX_BROWSER_STATE_BYTES {
            continue;
        }
        let normalized = match value {
            Value::Bool(true) => Some(key.clone()),
            Value::Bool(false) | Value::Null => None,
            Value::String(value) if value.eq_ignore_ascii_case("true") => Some(key.clone()),
            Value::String(value) if value.eq_ignore_ascii_case("false") => None,
            Value::String(value)
                if matches!(key.as_str(), "checked" | "pressed")
                    && value.eq_ignore_ascii_case("mixed") =>
            {
                Some(format!("{key}:mixed"))
            }
            Value::String(value)
                if key == "editable"
                    && matches!(
                        value.to_ascii_lowercase().as_str(),
                        "plaintext" | "richtext"
                    ) =>
            {
                Some("editable".to_owned())
            }
            _ => None,
        };
        if let Some(state) = normalized {
            if !states.contains(&state) {
                states.push(state);
            }
        }
    }
    Ok(states)
}

fn has_disallowed_control(value: &str, allow_layout_whitespace: bool) -> bool {
    value.chars().any(|character| {
        character.is_control()
            && !(allow_layout_whitespace && matches!(character, '\n' | '\r' | '\t'))
    })
}

fn backend_handle(value: &str) -> Result<String, BrowserNormalizeError> {
    if value.is_empty()
        || value.len() > MAX_BROWSER_BACKEND_HANDLE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(BrowserNormalizeError::InvalidBackendHandle);
    }
    Ok(value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn exact_binding_normalizes_without_logging_backend_capabilities() {
        let raw = json!({
            "status": "ok",
            "mode": "bind",
            "target_id": "target-secret",
            "binding_quality": "exact",
            "mutation_allowed": true,
            "tabs": [{
                "tab_id": "tab-secret",
                "title": "Example",
                "url": "https://example.com/private",
                "active": true
            }]
        });
        let normalized = normalize_cua_browser_binding(&raw).unwrap();
        assert_eq!(normalized.backend_target_id(), "target-secret");
        assert_eq!(normalized.tabs()[0].backend_tab_id(), "tab-secret");
        let debug = format!("{normalized:?}");
        assert!(!debug.contains("target-secret"));
        assert!(!debug.contains("tab-secret"));
        assert!(!debug.contains("example.com/private"));
    }

    #[test]
    fn heuristic_or_non_mutable_binding_never_becomes_action_authority() {
        let heuristic = json!({
            "status": "ok",
            "mode": "bind",
            "target_id": "target",
            "binding_quality": "heuristic",
            "mutation_allowed": false,
            "tabs": []
        });
        assert_eq!(
            normalize_cua_browser_binding(&heuristic),
            Err(BrowserNormalizeError::NonExactBinding)
        );
        let non_mutable = json!({
            "status": "ok",
            "mode": "bind",
            "target_id": "target",
            "binding_quality": "exact",
            "mutation_allowed": false,
            "tabs": []
        });
        assert_eq!(
            normalize_cua_browser_binding(&non_mutable),
            Err(BrowserNormalizeError::MutationNotAllowed)
        );
    }

    #[test]
    fn semantic_snapshot_preserves_only_known_actions() {
        let raw = snapshot(json!([
            {
                "ref": "p7:1",
                "role": "button",
                "name": "Save",
                "value": null,
                "states": {"disabled": false},
                "actions": ["click", "provider_secret_action"],
                "frame": "main",
                "visibility": "visible"
            },
            {
                "ref": "p7:2",
                "role": "textbox",
                "name": "Name",
                "value": "Alice",
                "states": {"editable": "plaintext", "focused": true},
                "actions": ["type"],
                "frame": "main",
                "visibility": "visible"
            }
        ]));
        let normalized = normalize_cua_browser_snapshot(&raw, "target", "tab").unwrap();
        assert_eq!(
            normalized.action_refs[0].actions,
            vec![BrowserAction::Click]
        );
        assert_eq!(normalized.action_refs[1].actions, vec![BrowserAction::Type]);
        assert!(normalized.outline.contains('\n'));
        let debug = format!("{normalized:?}");
        assert!(!debug.contains("p7:1"));
        assert!(!debug.contains("p7:2"));
        assert!(!debug.contains("continue-secret"));
    }

    #[test]
    fn exact_0193_state_object_maps_to_closed_neutral_states() {
        let raw = snapshot(json!([{
            "ref": "p7:1",
            "role": "textbox",
            "name": "Name",
            "value": "Alice",
            "states": {
                "editable": "plaintext",
                "focused": true,
                "required": false,
                "checked": "mixed",
                "provider_future_state": "secret"
            },
            "actions": ["type"],
            "frame": "main",
            "visibility": "in_viewport"
        }]));
        let normalized = normalize_cua_browser_snapshot(&raw, "target", "tab").unwrap();
        assert_eq!(
            normalized.action_refs[0].states,
            vec!["checked:mixed", "editable", "focused"]
        );
        assert!(!format!("{:?}", normalized.action_refs[0]).contains("provider_future_state"));
    }

    #[test]
    fn content_refs_never_gain_action_authority() {
        let mut raw = snapshot(json!([]));
        raw["content_refs"] = json!([{
            "ref": "p7:9",
            "role": "text",
            "name": "Article",
            "value": null,
            "states": {},
            "actions": ["click"],
            "frame": "main",
            "visibility": "visible"
        }]);
        let normalized = normalize_cua_browser_snapshot(&raw, "target", "tab").unwrap();
        assert!(normalized.content_refs[0].actions.is_empty());
    }

    #[test]
    fn snapshot_echo_must_match_the_exact_resolved_target_and_tab() {
        let raw = snapshot(json!([]));
        assert_eq!(
            normalize_cua_browser_snapshot(&raw, "other-target", "tab"),
            Err(BrowserNormalizeError::BackendIdentityMismatch)
        );
        assert_eq!(
            normalize_cua_browser_snapshot(&raw, "target", "other-tab"),
            Err(BrowserNormalizeError::BackendIdentityMismatch)
        );
    }

    #[test]
    fn unknown_only_action_ref_is_refused() {
        let raw = snapshot(json!([{
            "ref": "p7:1",
            "role": "button",
            "name": "Mystery",
            "value": null,
            "states": {},
            "actions": ["future_backend_action"],
            "frame": "main",
            "visibility": "visible"
        }]));
        assert_eq!(
            normalize_cua_browser_snapshot(&raw, "target", "tab"),
            Err(BrowserNormalizeError::InvalidActionRef)
        );
    }

    #[test]
    fn aggregate_snapshot_metadata_is_bounded_below_the_large_result_carrier() {
        let large_value = "x".repeat(MAX_BROWSER_VALUE_BYTES);
        let refs: Vec<Value> = (0..40)
            .map(|index| {
                json!({
                    "ref": format!("p1:{index}"),
                    "role": "textbox",
                    "name": "field",
                    "value": large_value,
                    "states": {},
                    "actions": ["type"],
                    "frame": "main",
                    "visibility": "visible"
                })
            })
            .collect();
        let value = json!({
            "status": "ok",
            "target_id": "target",
            "tab_id": "tab",
            "outline": "page",
            "refs": refs,
            "content_refs": [],
            "snapshot": {
                "format": "semantic_v2",
                "id": "snapshot",
                "complete": true,
                "continuation": null,
                "omitted": {"refs": 0, "content_refs": 0}
            }
        });
        assert!(serde_json::to_vec(&value).unwrap().len() > MAX_BROWSER_STRUCTURED_METADATA_BYTES);
        assert_eq!(
            normalize_cua_browser_snapshot(&value, "target", "tab"),
            Err(BrowserNormalizeError::ValueTooLarge)
        );
    }

    #[test]
    fn exact_cua_screenshot_metadata_accepts_integral_f64_viewport_dimensions() {
        let value = json!({
            "screenshot": {
                "mime_type": "image/png",
                "width": 1280,
                "height": 720,
                "coordinate_space": "viewport_css_px",
                "pixel_to_css_scale_x": 1.0,
                "pixel_to_css_scale_y": 1.0,
                "viewport_css_width": 1280.0,
                "viewport_css_height": 720.0,
                "source": "Page.captureScreenshot",
                "scope": "tab_content_viewport",
                "tab_activation": "preserved",
                "window_foregrounding": "not_requested"
            }
        });
        assert_eq!(
            normalize_screenshot_metadata(&value).unwrap(),
            Some((1280, 720, 1280, 720))
        );
    }

    #[test]
    fn screenshot_viewport_dimensions_reject_fractional_values() {
        let value = json!({
            "screenshot": {
                "mime_type": "image/png",
                "width": 1280,
                "height": 720,
                "coordinate_space": "viewport_css_px",
                "viewport_css_width": 1279.5,
                "viewport_css_height": 720.0
            }
        });
        assert_eq!(
            normalize_screenshot_metadata(&value),
            Err(BrowserNormalizeError::InvalidScreenshotMetadata)
        );
    }

    #[test]
    fn null_continuation_and_exact_screenshot_metadata_are_valid() {
        let mut raw = snapshot(json!([]));
        raw["snapshot"]["continuation"] = Value::Null;
        raw["screenshot"] = json!({
            "mime_type": "image/png",
            "coordinate_space": "viewport_css_px",
            "width": 1200,
            "height": 800,
            "viewport_css_width": 600,
            "viewport_css_height": 400
        });
        let normalized = normalize_cua_browser_snapshot(&raw, "target", "tab").unwrap();
        assert_eq!(normalized.backend_continuation(), None);
        assert_eq!(normalized.screenshot_width, Some(1200));
        assert_eq!(normalized.viewport_css_width, Some(600));

        raw["screenshot"]["width"] = json!(0);
        assert_eq!(
            normalize_cua_browser_snapshot(&raw, "target", "tab"),
            Err(BrowserNormalizeError::InvalidScreenshotMetadata)
        );
    }

    fn snapshot(refs: Value) -> Value {
        json!({
            "status": "ok",
            "mode": "snapshot",
            "target_id": "target",
            "tab_id": "tab",
            "snapshot": {
                "id": "p7",
                "format": "semantic_v2",
                "complete": true,
                "omitted": {
                    "css_hidden": 1,
                    "offscreen": 2,
                    "budget": 3
                },
                "continuation": "continue-secret"
            },
            "outline": "button Save\ntextbox Name",
            "refs": refs,
            "content_refs": []
        })
    }
}
