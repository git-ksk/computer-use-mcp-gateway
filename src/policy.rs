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

/// Conservative semantic classification for the Cua-compatible tool surface.
///
/// Unknown tools are classified as `dangerous` so newly discovered backend
/// capabilities never look safer than they have been reviewed to be. Exact
/// name allow/deny policy remains the enforcement boundary in V1.
pub fn classify_tool(tool_name: &str) -> ToolClass {
    match tool_name {
        "list_apps"
        | "list_windows"
        | "get_accessibility_tree"
        | "get_screen_size"
        | "get_cursor_position"
        | "get_window_state"
        | "screenshot"
        | "check_permissions"
        | "debug_window_info"
        | "zoom" => ToolClass::Observe,

        "click"
        | "right_click"
        | "double_click"
        | "drag"
        | "type_text"
        | "type_text_chars"
        | "press_key"
        | "hotkey"
        | "scroll"
        | "move_cursor"
        | "bring_to_front"
        | "set_agent_cursor"
        | "set_agent_cursor_position"
        | "set_agent_cursor_visible" => ToolClass::Interact,

        "launch_app" | "kill_app" => ToolClass::System,

        "shell_execute" | "run_javascript" => ToolClass::Dangerous,

        _ => ToolClass::Dangerous,
    }
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
    fn unknown_tools_are_conservatively_dangerous() {
        assert_eq!(classify_tool("future_backend_tool"), ToolClass::Dangerous);
    }
}
