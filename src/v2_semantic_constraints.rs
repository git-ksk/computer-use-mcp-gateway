//! Typed backend-neutral semantic authorization constraints for V2 northbound calls.
//!
//! This layer only narrows an already-authorized exact `DeviceCapability`. It never
//! grants a capability and never interprets provider-private identifiers.

use crate::v2_browser_runtime::BrowserBackendCommand;
use crate::v2_execution_safety::SemanticConstraintAdmissionEvidence;
use crate::v2_m0::{DeviceCapability, DeviceCommand, MAX_TYPE_TEXT_BYTES};
use reqwest::Url;
use ring::digest::{SHA256, digest};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

const MAX_POLICY_BYTES: usize = 64 * 1024;
const MAX_RULES: usize = 64;
const MAX_RULE_ID_BYTES: usize = 64;
const MAX_ALLOWED_ORIGINS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticConstraintError {
    InvalidPolicy,
    Denied,
    UnsupportedSubject,
}

impl std::fmt::Display for SemanticConstraintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidPolicy => "invalid semantic constraint policy",
            Self::Denied => "semantic constraint denied",
            Self::UnsupportedSubject => "semantic constraint subject unsupported",
        })
    }
}

impl std::error::Error for SemanticConstraintError {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticConstraintPolicyDocument {
    pub revision: u64,
    pub rules: Vec<SemanticConstraintRuleDocument>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SemanticConstraintRuleDocument {
    TypeTextMaxUtf8Bytes {
        rule_id: String,
        max_utf8_bytes: usize,
    },
    BrowserNavigateRequestedOrigins {
        rule_id: String,
        allowed_origins: Vec<String>,
    },
}

#[derive(Debug, Clone)]
enum SemanticConstraintRule {
    TypeTextMaxUtf8Bytes {
        rule_id: String,
        max_utf8_bytes: usize,
    },
    BrowserNavigateRequestedOrigins {
        rule_id: String,
        allowed_origins: HashSet<String>,
    },
}

impl SemanticConstraintRule {
    fn capability(&self) -> DeviceCapability {
        match self {
            Self::TypeTextMaxUtf8Bytes { .. } => DeviceCapability::TypeText,
            Self::BrowserNavigateRequestedOrigins { .. } => DeviceCapability::BrowserNavigate,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::TypeTextMaxUtf8Bytes { .. } => "type_text_max_utf8_bytes",
            Self::BrowserNavigateRequestedOrigins { .. } => "browser_navigate_requested_origins",
        }
    }

    fn rule_id(&self) -> &str {
        match self {
            Self::TypeTextMaxUtf8Bytes { rule_id, .. }
            | Self::BrowserNavigateRequestedOrigins { rule_id, .. } => rule_id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SemanticConstraintPolicy {
    revision: u64,
    digest: String,
    rules: HashMap<DeviceCapability, SemanticConstraintRule>,
}

impl SemanticConstraintPolicy {
    pub fn from_json(value: &str) -> Result<Self, SemanticConstraintError> {
        if value.is_empty() || value.len() > MAX_POLICY_BYTES {
            return Err(SemanticConstraintError::InvalidPolicy);
        }
        let document: SemanticConstraintPolicyDocument =
            serde_json::from_str(value).map_err(|_| SemanticConstraintError::InvalidPolicy)?;
        Self::from_document(document)
    }

    pub fn from_document(
        document: SemanticConstraintPolicyDocument,
    ) -> Result<Self, SemanticConstraintError> {
        if document.revision == 0 || document.rules.is_empty() || document.rules.len() > MAX_RULES {
            return Err(SemanticConstraintError::InvalidPolicy);
        }
        let canonical = serde_json::to_vec(&CanonicalPolicy::try_from(&document)?)
            .map_err(|_| SemanticConstraintError::InvalidPolicy)?;
        let hash = digest(&SHA256, &canonical);
        let mut snapshot_digest = String::with_capacity(64);
        for byte in hash.as_ref() {
            let _ = write!(&mut snapshot_digest, "{byte:02x}");
        }

        let mut rules = HashMap::new();
        for rule in document.rules {
            let rule = match rule {
                SemanticConstraintRuleDocument::TypeTextMaxUtf8Bytes {
                    rule_id,
                    max_utf8_bytes,
                } => {
                    validate_rule_id(&rule_id)?;
                    if max_utf8_bytes == 0 || max_utf8_bytes > MAX_TYPE_TEXT_BYTES {
                        return Err(SemanticConstraintError::InvalidPolicy);
                    }
                    SemanticConstraintRule::TypeTextMaxUtf8Bytes {
                        rule_id,
                        max_utf8_bytes,
                    }
                }
                SemanticConstraintRuleDocument::BrowserNavigateRequestedOrigins {
                    rule_id,
                    allowed_origins,
                } => {
                    validate_rule_id(&rule_id)?;
                    if allowed_origins.is_empty() || allowed_origins.len() > MAX_ALLOWED_ORIGINS {
                        return Err(SemanticConstraintError::InvalidPolicy);
                    }
                    let mut normalized = HashSet::new();
                    for origin in allowed_origins {
                        let origin = normalize_configured_origin(&origin)?;
                        if !normalized.insert(origin) {
                            return Err(SemanticConstraintError::InvalidPolicy);
                        }
                    }
                    SemanticConstraintRule::BrowserNavigateRequestedOrigins {
                        rule_id,
                        allowed_origins: normalized,
                    }
                }
            };
            if rules.insert(rule.capability(), rule).is_some() {
                return Err(SemanticConstraintError::InvalidPolicy);
            }
        }
        Ok(Self {
            revision: document.revision,
            digest: snapshot_digest,
            rules,
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn has_rule_for(&self, capability: DeviceCapability) -> bool {
        self.rules.contains_key(&capability)
    }

    pub fn evaluate(
        &self,
        command: &DeviceCommand,
    ) -> Result<Option<SemanticConstraintAdmissionEvidence>, SemanticConstraintError> {
        let capability = command.capability();
        let Some(rule) = self.rules.get(&capability) else {
            return Ok(None);
        };
        match rule {
            SemanticConstraintRule::TypeTextMaxUtf8Bytes { max_utf8_bytes, .. } => {
                let text = match command {
                    DeviceCommand::TypeText { text }
                    | DeviceCommand::TypeTextAdvanced { text, .. } => text,
                    _ => return Err(SemanticConstraintError::UnsupportedSubject),
                };
                if text.len() > *max_utf8_bytes {
                    return Err(SemanticConstraintError::Denied);
                }
            }
            SemanticConstraintRule::BrowserNavigateRequestedOrigins {
                allowed_origins, ..
            } => {
                let url = match command {
                    DeviceCommand::Browser {
                        command: BrowserBackendCommand::Navigate { url, .. },
                    } => url,
                    _ => return Err(SemanticConstraintError::UnsupportedSubject),
                };
                let requested_origin = normalize_requested_origin(url)?;
                if !allowed_origins.contains(&requested_origin) {
                    return Err(SemanticConstraintError::Denied);
                }
            }
        }
        Ok(Some(SemanticConstraintAdmissionEvidence {
            revision: self.revision,
            snapshot_digest: self.digest.clone(),
            kind: rule.kind().to_owned(),
            rule_id: rule.rule_id().to_owned(),
        }))
    }
}

fn validate_rule_id(value: &str) -> Result<(), SemanticConstraintError> {
    if value.is_empty()
        || value.len() > MAX_RULE_ID_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(SemanticConstraintError::InvalidPolicy);
    }
    Ok(())
}

fn normalize_configured_origin(value: &str) -> Result<String, SemanticConstraintError> {
    let url = Url::parse(value).map_err(|_| SemanticConstraintError::InvalidPolicy)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
    {
        return Err(SemanticConstraintError::InvalidPolicy);
    }
    Ok(url.origin().ascii_serialization())
}

fn normalize_requested_origin(value: &str) -> Result<String, SemanticConstraintError> {
    let url = Url::parse(value).map_err(|_| SemanticConstraintError::UnsupportedSubject)?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(SemanticConstraintError::UnsupportedSubject);
    }
    Ok(url.origin().ascii_serialization())
}

#[derive(serde::Serialize)]
struct CanonicalPolicy {
    revision: u64,
    rules: Vec<CanonicalRule>,
}

#[derive(serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum CanonicalRule {
    TypeTextMaxUtf8Bytes {
        rule_id: String,
        max_utf8_bytes: usize,
    },
    BrowserNavigateRequestedOrigins {
        rule_id: String,
        allowed_origins: Vec<String>,
    },
}

impl TryFrom<&SemanticConstraintPolicyDocument> for CanonicalPolicy {
    type Error = SemanticConstraintError;

    fn try_from(value: &SemanticConstraintPolicyDocument) -> Result<Self, Self::Error> {
        let mut rules = Vec::with_capacity(value.rules.len());
        for rule in &value.rules {
            rules.push(match rule {
                SemanticConstraintRuleDocument::TypeTextMaxUtf8Bytes {
                    rule_id,
                    max_utf8_bytes,
                } => CanonicalRule::TypeTextMaxUtf8Bytes {
                    rule_id: rule_id.clone(),
                    max_utf8_bytes: *max_utf8_bytes,
                },
                SemanticConstraintRuleDocument::BrowserNavigateRequestedOrigins {
                    rule_id,
                    allowed_origins,
                } => {
                    let mut normalized = allowed_origins
                        .iter()
                        .map(|origin| normalize_configured_origin(origin))
                        .collect::<Result<Vec<_>, _>>()?;
                    normalized.sort();
                    CanonicalRule::BrowserNavigateRequestedOrigins {
                        rule_id: rule_id.clone(),
                        allowed_origins: normalized,
                    }
                }
            });
        }
        rules.sort_by(|left, right| {
            let left = serde_json::to_string(left).unwrap_or_default();
            let right = serde_json::to_string(right).unwrap_or_default();
            left.cmp(&right)
        });
        Ok(Self {
            revision: value.revision,
            rules,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_browser_runtime::BrowserBackendCommand;
    use crate::v2_m0::{DeviceCommand, InputDeliveryMode, InputTarget};

    fn policy() -> SemanticConstraintPolicy {
        SemanticConstraintPolicy::from_json(
            r#"{
                "revision": 7,
                "rules": [
                    {"kind":"type_text_max_utf8_bytes","rule_id":"text-small","max_utf8_bytes":5},
                    {"kind":"browser_navigate_requested_origins","rule_id":"nav-prod","allowed_origins":["https://example.com","http://localhost:3000"]}
                ]
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn type_text_is_checked_on_final_utf8_bytes() {
        let policy = policy();
        let allowed = DeviceCommand::TypeTextAdvanced {
            context_id: None,
            text: "éé".into(),
            target: InputTarget::Desktop,
            delivery: InputDeliveryMode::Foreground,
            delay_ms: 30,
        };
        let evidence = policy.evaluate(&allowed).unwrap().unwrap();
        assert_eq!(evidence.revision, 7);
        assert_eq!(evidence.kind, "type_text_max_utf8_bytes");
        let denied = DeviceCommand::TypeText {
            text: "ééé".into()
        };
        assert_eq!(
            policy.evaluate(&denied),
            Err(SemanticConstraintError::Denied)
        );
    }

    #[test]
    fn browser_navigation_matches_normalized_requested_origin_only() {
        let policy = policy();
        let allowed = DeviceCommand::Browser {
            command: BrowserBackendCommand::Navigate {
                context_id: "ctx".into(),
                backend_target_id: "target".into(),
                backend_tab_id: "tab".into(),
                url: "https://example.com:443/path?q=1".into(),
            },
        };
        assert!(policy.evaluate(&allowed).unwrap().is_some());
        let denied = DeviceCommand::Browser {
            command: BrowserBackendCommand::Navigate {
                context_id: "ctx".into(),
                backend_target_id: "target".into(),
                backend_tab_id: "tab".into(),
                url: "https://evil.example/path".into(),
            },
        };
        assert_eq!(
            policy.evaluate(&denied),
            Err(SemanticConstraintError::Denied)
        );
        let opaque = DeviceCommand::Browser {
            command: BrowserBackendCommand::Navigate {
                context_id: "ctx".into(),
                backend_target_id: "target".into(),
                backend_tab_id: "tab".into(),
                url: "about:blank".into(),
            },
        };
        assert_eq!(
            policy.evaluate(&opaque),
            Err(SemanticConstraintError::UnsupportedSubject)
        );
    }

    #[test]
    fn canonical_digest_is_order_and_origin_spelling_stable() {
        let first = policy();
        let second = SemanticConstraintPolicy::from_json(
            r#"{"revision":7,"rules":[
                {"kind":"browser_navigate_requested_origins","rule_id":"nav-prod","allowed_origins":["http://localhost:3000/","https://example.com:443/"]},
                {"kind":"type_text_max_utf8_bytes","rule_id":"text-small","max_utf8_bytes":5}
            ]}"#,
        )
        .unwrap();
        assert_eq!(first.digest(), second.digest());
    }

    #[test]
    fn malformed_or_ambiguous_policy_fails_closed() {
        for value in [
            r#"{"revision":0,"rules":[]}"#,
            r#"{"revision":1,"rules":[{"kind":"type_text_max_utf8_bytes","rule_id":"bad id","max_utf8_bytes":1}]}"#,
            r#"{"revision":1,"rules":[{"kind":"browser_navigate_requested_origins","rule_id":"nav","allowed_origins":["https://example.com/path"]}]}"#,
            r#"{"revision":1,"rules":[{"kind":"type_text_max_utf8_bytes","rule_id":"a","max_utf8_bytes":1},{"kind":"type_text_max_utf8_bytes","rule_id":"b","max_utf8_bytes":2}]}"#,
            r#"{"revision":1,"rules":[{"kind":"type_text_max_utf8_bytes","rule_id":"a","max_utf8_bytes":1,"typo_ceiling":2}]}"#,
            r#"{"revision":1,"rules":[{"kind":"type_text_max_utf8_bytes","rule_id":"a","max_utf8_bytes":1}],"unknown_top_level":true}"#,
        ] {
            assert!(SemanticConstraintPolicy::from_json(value).is_err());
        }
    }
}
