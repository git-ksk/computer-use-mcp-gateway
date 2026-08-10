use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
}

/// Tool policy applied before a backend call is forwarded.
///
/// Deny entries always win. When an allowlist is configured, tools not present
/// in it are denied. An empty allowlist means all discovered backend tools are
/// eligible unless explicitly denied.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    allowed: Option<HashSet<String>>,
    denied: HashSet<String>,
}

impl ToolPolicy {
    pub fn new(allowed: Vec<String>, denied: Vec<String>) -> Self {
        let allowed = if allowed.is_empty() {
            None
        } else {
            Some(allowed.into_iter().collect())
        };

        Self {
            allowed,
            denied: denied.into_iter().collect(),
        }
    }

    pub fn evaluate(&self, tool_name: &str) -> PolicyDecision {
        if self.denied.contains(tool_name) {
            return PolicyDecision::Deny;
        }

        match &self.allowed {
            Some(allowed) if !allowed.contains(tool_name) => PolicyDecision::Deny,
            _ => PolicyDecision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_deny_wins() {
        let policy = ToolPolicy::new(
            vec!["shell".into(), "screenshot".into()],
            vec!["shell".into()],
        );
        assert_eq!(policy.evaluate("shell"), PolicyDecision::Deny);
        assert_eq!(policy.evaluate("screenshot"), PolicyDecision::Allow);
    }

    #[test]
    fn allowlist_is_fail_closed_when_present() {
        let policy = ToolPolicy::new(vec!["screenshot".into()], vec![]);
        assert_eq!(policy.evaluate("screenshot"), PolicyDecision::Allow);
        assert_eq!(policy.evaluate("click"), PolicyDecision::Deny);
    }

    #[test]
    fn empty_allowlist_allows_discovered_tools() {
        let policy = ToolPolicy::new(vec![], vec![]);
        assert_eq!(policy.evaluate("click"), PolicyDecision::Allow);
    }
}
