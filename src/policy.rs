use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
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
        let allowed = allowed
            .into_iter()
            .filter(|tool| tool != "*")
            .collect();

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_deny_wins() {
        let policy = ToolPolicy::new(
            vec!["*".into(), "screenshot".into()],
            vec!["shell".into()],
        );
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
}
