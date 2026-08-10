#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyDecision {
    Allow,
    Deny,
}

/// V1 starts fail-closed for explicit deny entries. A richer classifier is M3.
#[derive(Debug, Clone, Default)]
pub struct ToolPolicy {
    denied: Vec<String>,
}

impl ToolPolicy {
    pub fn new(denied: Vec<String>) -> Self {
        Self { denied }
    }

    pub fn evaluate(&self, tool_name: &str) -> PolicyDecision {
        if self.denied.iter().any(|name| name == tool_name) {
            PolicyDecision::Deny
        } else {
            PolicyDecision::Allow
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_deny_wins() {
        let policy = ToolPolicy::new(vec!["shell".into()]);
        assert_eq!(policy.evaluate("shell"), PolicyDecision::Deny);
        assert_eq!(policy.evaluate("screenshot"), PolicyDecision::Allow);
    }
}
