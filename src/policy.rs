use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolClass {
    Observe,
    Interact,
    System,
    Dangerous,
}

impl ToolClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Interact => "interact",
            Self::System => "system",
            Self::Dangerous => "dangerous",
        }
    }
}

/// Reviewed semantic classification for the Cua-compatible tool surface.
///
/// `known_tool_class` returns `Some` only for capabilities that have been
/// intentionally reviewed. `classify_tool` preserves the V1 fail-closed rule by
/// treating every genuinely unknown future capability as `dangerous`.
///
/// The pinned Cua 0.19.3 tool fixture is checked in unit tests and in the
/// real-Cua smoke so a backend tool-surface change cannot silently become an
/// "unknown => dangerous" audit label without an explicit review.
pub fn known_tool_class(tool_name: &str) -> Option<ToolClass> {
    let class = match tool_name {
        // Read-only observation / inspection.
        "list_apps"
        | "list_windows"
        | "get_window_state"
        | "verify_state"
        | "clipboard_read"
        | "get_screen_size"
        | "get_desktop_state"
        | "get_cursor_position"
        | "get_agent_cursor_state"
        | "health_report"
        | "get_config"
        | "get_accessibility_tree"
        | "zoom"
        | "get_browser_state"
        | "get_recording_state"
        | "get_session_state"
        | "check_for_update"
        // Retained compatibility names from older reviewed Cua surfaces.
        | "screenshot"
        | "debug_window_info" => ToolClass::Observe,

        // Direct UI/browser interaction whose primary effect is on the current
        // desktop/browser/session rather than installing software or executing
        // an open-ended command surface.
        "bring_to_front"
        | "set_window_frame"
        | "click"
        | "double_click"
        | "right_click"
        | "drag"
        | "type_text"
        | "press_key"
        | "hotkey"
        | "set_value"
        | "scroll"
        | "move_cursor"
        | "set_agent_cursor_enabled"
        | "set_agent_cursor_motion"
        | "set_agent_cursor_theme"
        | "browser_navigate"
        | "browser_click"
        | "browser_type"
        | "browser_dialog"
        | "browser_pointer"
        // Platform-specific low-level mouse primitives exposed by pinned Cua
        // 0.19.3 on Linux.
        | "mouse_button_down"
        | "mouse_button_up"
        | "mouse_drag"
        | "parallel_mouse_drag"
        // Retained compatibility names from older reviewed Cua surfaces.
        | "type_text_chars"
        | "set_agent_cursor"
        | "set_agent_cursor_position"
        | "set_agent_cursor_visible" => ToolClass::Interact,

        // Process/driver/session lifecycle and local machine configuration.
        "launch_app"
        | "kill_app"
        | "clipboard_write"
        | "check_permissions"
        | "set_config"
        | "browser_prepare"
        | "start_recording"
        | "stop_recording"
        | "start_session"
        | "end_session" => ToolClass::System,

        // Explicitly reviewed high-risk or broad-effect capabilities. These are
        // deliberately `dangerous`; unlike the fallback below, they are known.
        "invoke_menu"
        | "page"
        | "browser_set_input_files"
        | "browser_download"
        | "replay_trajectory"
        | "install_ffmpeg"
        | "escalate_session"
        // Retained compatibility names from older reviewed Cua surfaces.
        | "shell_execute"
        | "run_javascript" => ToolClass::Dangerous,

        _ => return None,
    };
    Some(class)
}

pub fn classify_tool(tool_name: &str) -> ToolClass {
    known_tool_class(tool_name).unwrap_or(ToolClass::Dangerous)
}

/// Tool policy applied before a backend call is forwarded.
///
/// V1 is deny-by-default. Operators must explicitly allow tools by name or use
/// `*` to opt in to every discovered backend tool. Deny entries always win.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    allowed: HashSet<String>,
    allow_all: bool,
    denied: HashSet<String>,
}

impl ToolPolicy {
    pub fn new(allowed: Vec<String>, denied: Vec<String>) -> Self {
        let allow_all = allowed.iter().any(|tool| tool == "*");
        let allowed = allowed.into_iter().filter(|tool| tool != "*").collect();

        Self {
            allowed,
            allow_all,
            denied: denied.into_iter().collect(),
        }
    }

    pub fn evaluate(&self, tool_name: &str) -> PolicyDecision {
        if self.denied.contains(tool_name) {
            return PolicyDecision::Deny;
        }

        if self.allow_all || self.allowed.contains(tool_name) {
            PolicyDecision::Allow
        } else {
            PolicyDecision::Deny
        }
    }

    pub fn classify(&self, tool_name: &str) -> ToolClass {
        classify_tool(tool_name)
    }

    /// Whether this policy can expose at least one effectful backend capability.
    /// A wildcard is conservatively effectful because future/unknown tools are
    /// classified as dangerous. Explicitly denied names never create authority.
    pub fn may_allow_effectful(&self) -> bool {
        self.allow_all
            || self.allowed.iter().any(|tool| {
                !self.denied.contains(tool) && classify_tool(tool) != ToolClass::Observe
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_deny_wins() {
        let policy = ToolPolicy::new(vec!["*".into(), "screenshot".into()], vec!["shell".into()]);
        assert_eq!(policy.evaluate("shell"), PolicyDecision::Deny);
        assert_eq!(policy.evaluate("screenshot"), PolicyDecision::Allow);
    }

    #[test]
    fn explicit_allowlist_is_fail_closed() {
        let policy = ToolPolicy::new(vec!["screenshot".into()], vec![]);
        assert_eq!(policy.evaluate("screenshot"), PolicyDecision::Allow);
        assert_eq!(policy.evaluate("click"), PolicyDecision::Deny);
    }

    #[test]
    fn empty_allowlist_denies_everything() {
        let policy = ToolPolicy::new(vec![], vec![]);
        assert_eq!(policy.evaluate("click"), PolicyDecision::Deny);
    }

    #[test]
    fn wildcard_requires_explicit_opt_in() {
        let policy = ToolPolicy::new(vec!["*".into()], vec![]);
        assert_eq!(policy.evaluate("click"), PolicyDecision::Allow);
        assert_eq!(policy.evaluate("future_tool"), PolicyDecision::Allow);
    }

    #[test]
    fn semantic_classes_cover_known_risk_levels() {
        assert_eq!(classify_tool("list_windows"), ToolClass::Observe);
        assert_eq!(classify_tool("click"), ToolClass::Interact);
        assert_eq!(classify_tool("kill_app"), ToolClass::System);
        assert_eq!(classify_tool("shell_execute"), ToolClass::Dangerous);
    }

    #[test]
    fn pinned_cua_0_19_3_surface_is_explicitly_reviewed() {
        let fixtures = [
            include_str!("../tests/fixtures/cua-0.19.3-tools.txt"),
            include_str!("../tests/fixtures/cua-0.19.3-tools-linux-extra.txt"),
            include_str!("../tests/fixtures/cua-0.19.3-tools-windows-extra.txt"),
        ];
        let mut count = 0usize;
        for tool_name in fixtures
            .into_iter()
            .flat_map(str::lines)
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            count += 1;
            assert!(
                known_tool_class(tool_name).is_some(),
                "pinned Cua tool is missing an explicit semantic class: {tool_name}"
            );
        }
        assert_eq!(count, 59, "unexpected reviewed Cua 0.19.3 tool union size");
    }

    #[test]
    fn explicitly_dangerous_tools_are_distinct_from_unknown_tools() {
        assert_eq!(
            known_tool_class("replay_trajectory"),
            Some(ToolClass::Dangerous)
        );
        assert_eq!(known_tool_class("future_backend_tool"), None);
        assert_eq!(classify_tool("future_backend_tool"), ToolClass::Dangerous);
    }

    #[test]
    fn unknown_tools_are_conservatively_dangerous() {
        assert_eq!(classify_tool("future_backend_tool"), ToolClass::Dangerous);
    }

    #[test]
    fn effectful_allowance_is_conservative_and_respects_explicit_deny() {
        assert!(!ToolPolicy::new(vec![], vec![]).may_allow_effectful());
        assert!(!ToolPolicy::new(vec!["list_windows".into()], vec![]).may_allow_effectful());
        assert!(ToolPolicy::new(vec!["click".into()], vec![]).may_allow_effectful());
        assert!(ToolPolicy::new(vec!["future_backend_tool".into()], vec![]).may_allow_effectful());
        assert!(ToolPolicy::new(vec!["*".into()], vec![]).may_allow_effectful());
        assert!(!ToolPolicy::new(vec!["click".into()], vec!["click".into()]).may_allow_effectful());
    }
}
