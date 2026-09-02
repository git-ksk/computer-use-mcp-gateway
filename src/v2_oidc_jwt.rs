//! Provider-neutral signed OIDC/JWT access-token verification for the V2 northbound boundary.
//!
//! Provider-specific logic terminates here. A verified token is reduced to the existing
//! `AuthenticatedClientPrincipal { issuer, subject }` plus scopes before CUMG authorization.

use crate::v2_m0_trust::AuthenticatedClientPrincipal;
use crate::v2_m1_northbound::{AccessTokenVerifier, TokenVerificationError, VerifiedAccessToken};
use async_trait::async_trait;
use jsonwebtoken::jwk::{Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use reqwest::{Client, Url, redirect::Policy as RedirectPolicy};
use serde::Deserialize;
use std::{
    collections::HashSet,
    fmt,
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const MAX_JWKS_BYTES: usize = 256 * 1024;
const MAX_JWKS_KEYS: usize = 64;
const MAX_KID_BYTES: usize = 256;
const MAX_TOKEN_BYTES: usize = 32 * 1024;
const MAX_AUDIENCE_BYTES: usize = 2 * 1024;
const MAX_SCOPE_BYTES: usize = 8 * 1024;
const MAX_SCOPES: usize = 128;
const MAX_CLOCK_SKEW_SECS: u64 = 300;
const MAX_CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const MAX_HTTP_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OidcJwtAlgorithm {
    Rs256,
    Rs384,
    Rs512,
    Ps256,
    Ps384,
    Ps512,
    Es256,
    Es384,
    EdDsa,
}

impl OidcJwtAlgorithm {
    fn algorithm(self) -> Algorithm {
        match self {
            Self::Rs256 => Algorithm::RS256,
            Self::Rs384 => Algorithm::RS384,
            Self::Rs512 => Algorithm::RS512,
            Self::Ps256 => Algorithm::PS256,
            Self::Ps384 => Algorithm::PS384,
            Self::Ps512 => Algorithm::PS512,
            Self::Es256 => Algorithm::ES256,
            Self::Es384 => Algorithm::ES384,
            Self::EdDsa => Algorithm::EdDSA,
        }
    }
}

impl fmt::Display for OidcJwtAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Rs256 => "RS256",
            Self::Rs384 => "RS384",
            Self::Rs512 => "RS512",
            Self::Ps256 => "PS256",
            Self::Ps384 => "PS384",
            Self::Ps512 => "PS512",
            Self::Es256 => "ES256",
            Self::Es384 => "ES384",
            Self::EdDsa => "EdDSA",
        };
        f.write_str(value)
    }
}

impl FromStr for OidcJwtAlgorithm {
    type Err = TokenVerificationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "RS256" => Ok(Self::Rs256),
            "RS384" => Ok(Self::Rs384),
            "RS512" => Ok(Self::Rs512),
            "PS256" => Ok(Self::Ps256),
            "PS384" => Ok(Self::Ps384),
            "PS512" => Ok(Self::Ps512),
            "ES256" => Ok(Self::Es256),
            "ES384" => Ok(Self::Es384),
            "EdDSA" => Ok(Self::EdDsa),
            // Deliberately reject HMAC/symmetric and `none`-style values.
            _ => Err(TokenVerificationError::InvalidConfiguration),
        }
    }
}

#[derive(Clone)]
pub struct OidcJwtConfig {
    pub issuer: String,
    pub audience: String,
    pub jwks_uri: String,
    pub allowed_algorithms: Vec<OidcJwtAlgorithm>,
    pub clock_skew: Duration,
    pub jwks_cache_ttl: Duration,
    pub unknown_kid_refresh_interval: Duration,
    pub http_timeout: Duration,
}

impl fmt::Debug for OidcJwtConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcJwtConfig")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("jwks_uri", &self.jwks_uri)
            .field("allowed_algorithms", &self.allowed_algorithms)
            .field("clock_skew", &self.clock_skew)
            .field("jwks_cache_ttl", &self.jwks_cache_ttl)
            .field(
                "unknown_kid_refresh_interval",
                &self.unknown_kid_refresh_interval,
            )
            .field("http_timeout", &self.http_timeout)
            .finish()
    }
}

impl OidcJwtConfig {
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        jwks_uri: impl Into<String>,
        allowed_algorithms: Vec<OidcJwtAlgorithm>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            audience: audience.into(),
            jwks_uri: jwks_uri.into(),
            allowed_algorithms,
            clock_skew: Duration::from_secs(30),
            jwks_cache_ttl: Duration::from_secs(300),
            unknown_kid_refresh_interval: Duration::from_secs(30),
            http_timeout: Duration::from_secs(5),
        }
    }

    fn validate(&self) -> Result<Url, TokenVerificationError> {
        validate_issuer_url(&self.issuer)?;
        validate_audience(&self.audience)?;
        let jwks_uri = validate_jwks_url(&self.jwks_uri)?;
        if self.allowed_algorithms.is_empty()
            || self.allowed_algorithms.iter().collect::<HashSet<_>>().len()
                != self.allowed_algorithms.len()
            || self.clock_skew > Duration::from_secs(MAX_CLOCK_SKEW_SECS)
            || self.jwks_cache_ttl.is_zero()
            || self.jwks_cache_ttl > Duration::from_secs(MAX_CACHE_TTL_SECS)
            || self.unknown_kid_refresh_interval.is_zero()
            || self.unknown_kid_refresh_interval > self.jwks_cache_ttl
            || self.http_timeout.is_zero()
            || self.http_timeout > Duration::from_secs(MAX_HTTP_TIMEOUT_SECS)
        {
            return Err(TokenVerificationError::InvalidConfiguration);
        }
        Ok(jwks_uri)
    }
}

fn validate_issuer_url(value: &str) -> Result<Url, TokenVerificationError> {
    let url = validate_https_url(value)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(TokenVerificationError::InvalidConfiguration);
    }
    Ok(url)
}

fn validate_audience(value: &str) -> Result<(), TokenVerificationError> {
    if value.trim().is_empty()
        || value.len() > MAX_AUDIENCE_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(TokenVerificationError::InvalidConfiguration);
    }
    Ok(())
}

fn validate_jwks_url(value: &str) -> Result<Url, TokenVerificationError> {
    let url = validate_https_url(value)?;
    if url.fragment().is_some() {
        return Err(TokenVerificationError::InvalidConfiguration);
    }
    Ok(url)
}

fn validate_https_url(value: &str) -> Result<Url, TokenVerificationError> {
    let url = Url::parse(value).map_err(|_| TokenVerificationError::InvalidConfiguration)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(TokenVerificationError::InvalidConfiguration);
    }
    Ok(url)
}

#[async_trait]
trait JwksFetcher: Send + Sync {
    async fn fetch(&self) -> Result<JwkSet, TokenVerificationError>;
}

struct HttpJwksFetcher {
    client: Client,
    uri: Url,
}

#[async_trait]
impl JwksFetcher for HttpJwksFetcher {
    async fn fetch(&self) -> Result<JwkSet, TokenVerificationError> {
        let mut response = self
            .client
            .get(self.uri.clone())
            .send()
            .await
            .map_err(|_| TokenVerificationError::Unavailable)?;
        if !response.status().is_success() {
            return Err(TokenVerificationError::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|len| len > MAX_JWKS_BYTES as u64)
        {
            return Err(TokenVerificationError::Unavailable);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TokenVerificationError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_JWKS_BYTES {
                return Err(TokenVerificationError::Unavailable);
            }
            body.extend_from_slice(&chunk);
        }
        let set: JwkSet =
            serde_json::from_slice(&body).map_err(|_| TokenVerificationError::Unavailable)?;
        validate_jwks_set(&set)?;
        Ok(set)
    }
}

fn validate_jwks_set(set: &JwkSet) -> Result<(), TokenVerificationError> {
    if set.keys.is_empty() || set.keys.len() > MAX_JWKS_KEYS {
        return Err(TokenVerificationError::Unavailable);
    }
    let mut kids = HashSet::new();
    for key in &set.keys {
        if let Some(kid) = key.common.key_id.as_deref() {
            if kid.is_empty() || kid.len() > MAX_KID_BYTES || !kids.insert(kid) {
                return Err(TokenVerificationError::Unavailable);
            }
        }
    }
    Ok(())
}

#[derive(Default)]
struct JwksCache {
    set: Option<JwkSet>,
    fetched_at: Option<Instant>,
    last_refresh_attempt: Option<Instant>,
}

#[derive(Clone)]
pub struct OidcJwtVerifier {
    config: OidcJwtConfig,
    allowed_algorithms: HashSet<Algorithm>,
    fetcher: Arc<dyn JwksFetcher>,
    cache: Arc<Mutex<JwksCache>>,
}

impl OidcJwtVerifier {
    pub fn new(config: OidcJwtConfig) -> Result<Self, TokenVerificationError> {
        let jwks_uri = config.validate()?;
        let client = Client::builder()
            .redirect(RedirectPolicy::none())
            .timeout(config.http_timeout)
            .build()
            .map_err(|_| TokenVerificationError::InvalidConfiguration)?;
        let allowed_algorithms = config
            .allowed_algorithms
            .iter()
            .map(|alg| alg.algorithm())
            .collect();
        Ok(Self {
            config,
            allowed_algorithms,
            fetcher: Arc::new(HttpJwksFetcher {
                client,
                uri: jwks_uri,
            }),
            cache: Arc::new(Mutex::new(JwksCache::default())),
        })
    }

    #[cfg(test)]
    fn new_with_fetcher(
        config: OidcJwtConfig,
        fetcher: Arc<dyn JwksFetcher>,
    ) -> Result<Self, TokenVerificationError> {
        config.validate()?;
        let allowed_algorithms = config
            .allowed_algorithms
            .iter()
            .map(|alg| alg.algorithm())
            .collect();
        Ok(Self {
            config,
            allowed_algorithms,
            fetcher,
            cache: Arc::new(Mutex::new(JwksCache::default())),
        })
    }

    async fn decoding_key_for(
        &self,
        kid: &str,
        algorithm: Algorithm,
    ) -> Result<DecodingKey, TokenVerificationError> {
        let mut cache = self.cache.lock().await;
        let now = Instant::now();
        let cache_fresh = cache
            .fetched_at
            .is_some_and(|at| now.saturating_duration_since(at) < self.config.jwks_cache_ttl);
        if !cache_fresh {
            self.refresh_locked(&mut cache, now).await?;
        }
        if let Some(key) = cache.set.as_ref().and_then(|set| set.find(kid)).cloned() {
            return decoding_key_from_jwk(&key, algorithm);
        }

        let can_refresh_unknown = cache.last_refresh_attempt.is_none_or(|at| {
            now.saturating_duration_since(at) >= self.config.unknown_kid_refresh_interval
        });
        if can_refresh_unknown {
            self.refresh_locked(&mut cache, now).await?;
            if let Some(key) = cache.set.as_ref().and_then(|set| set.find(kid)).cloned() {
                return decoding_key_from_jwk(&key, algorithm);
            }
        }
        Err(TokenVerificationError::InvalidToken)
    }

    async fn refresh_locked(
        &self,
        cache: &mut JwksCache,
        now: Instant,
    ) -> Result<(), TokenVerificationError> {
        cache.last_refresh_attempt = Some(now);
        let set = self.fetcher.fetch().await?;
        validate_jwks_set(&set)?;
        cache.set = Some(set);
        cache.fetched_at = Some(Instant::now());
        Ok(())
    }

    fn validation(&self, algorithm: Algorithm) -> Validation {
        let mut validation = Validation::new(algorithm);
        validation.required_spec_claims = ["exp", "iss", "aud", "sub"]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
        validation.validate_exp = true;
        validation.validate_nbf = true;
        validation.leeway = self.config.clock_skew.as_secs();
        validation.set_issuer(&[self.config.issuer.as_str()]);
        validation.set_audience(&[self.config.audience.as_str()]);
        validation
    }
}

fn decoding_key_from_jwk(
    jwk: &Jwk,
    algorithm: Algorithm,
) -> Result<DecodingKey, TokenVerificationError> {
    if jwk
        .common
        .public_key_use
        .as_ref()
        .is_some_and(|use_| *use_ != PublicKeyUse::Signature)
    {
        return Err(TokenVerificationError::InvalidToken);
    }
    if jwk
        .common
        .key_operations
        .as_ref()
        .is_some_and(|ops| !ops.contains(&KeyOperations::Verify))
    {
        return Err(TokenVerificationError::InvalidToken);
    }
    if let Some(key_algorithm) = jwk.common.key_algorithm.as_ref() {
        let Some(jwk_algorithm) = signing_algorithm_from_jwk(key_algorithm) else {
            return Err(TokenVerificationError::InvalidToken);
        };
        if jwk_algorithm != algorithm {
            return Err(TokenVerificationError::InvalidToken);
        }
    }
    DecodingKey::from_jwk(jwk).map_err(|_| TokenVerificationError::InvalidToken)
}

fn signing_algorithm_from_jwk(value: &KeyAlgorithm) -> Option<Algorithm> {
    match value {
        KeyAlgorithm::RS256 => Some(Algorithm::RS256),
        KeyAlgorithm::RS384 => Some(Algorithm::RS384),
        KeyAlgorithm::RS512 => Some(Algorithm::RS512),
        KeyAlgorithm::PS256 => Some(Algorithm::PS256),
        KeyAlgorithm::PS384 => Some(Algorithm::PS384),
        KeyAlgorithm::PS512 => Some(Algorithm::PS512),
        KeyAlgorithm::ES256 => Some(Algorithm::ES256),
        KeyAlgorithm::ES384 => Some(Algorithm::ES384),
        KeyAlgorithm::EdDSA => Some(Algorithm::EdDSA),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
struct OidcClaims {
    sub: String,
    #[allow(dead_code)]
    iss: String,
    #[allow(dead_code)]
    aud: serde_json::Value,
    #[allow(dead_code)]
    exp: u64,
    #[serde(default)]
    #[allow(dead_code)]
    nbf: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

#[async_trait]
impl AccessTokenVerifier for OidcJwtVerifier {
    fn unavailable_error_code(&self) -> &'static str {
        "oidc_jwks_unavailable"
    }

    async fn verify(&self, token: &str) -> Result<VerifiedAccessToken, TokenVerificationError> {
        if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
            return Err(TokenVerificationError::InvalidToken);
        }
        let header = decode_header(token).map_err(|_| TokenVerificationError::InvalidToken)?;
        if !self.allowed_algorithms.contains(&header.alg) {
            return Err(TokenVerificationError::InvalidToken);
        }
        let kid = header
            .kid
            .as_deref()
            .filter(|kid| !kid.is_empty() && kid.len() <= MAX_KID_BYTES)
            .ok_or(TokenVerificationError::InvalidToken)?;
        let key = self.decoding_key_for(kid, header.alg).await?;
        let token_data = decode::<OidcClaims>(token, &key, &self.validation(header.alg))
            .map_err(|_| TokenVerificationError::InvalidToken)?;
        if token_data.claims.sub.trim().is_empty() {
            return Err(TokenVerificationError::InvalidToken);
        }
        let scope = token_data.claims.scope.unwrap_or_default();
        if scope.len() > MAX_SCOPE_BYTES {
            return Err(TokenVerificationError::InvalidToken);
        }
        let scopes = scope
            .split_ascii_whitespace()
            .map(ToOwned::to_owned)
            .collect::<HashSet<_>>();
        if scopes.len() > MAX_SCOPES {
            return Err(TokenVerificationError::InvalidToken);
        }
        let principal =
            AuthenticatedClientPrincipal::new(self.config.issuer.clone(), token_data.claims.sub)
                .map_err(|_| TokenVerificationError::InvalidToken)?;
        Ok(VerifiedAccessToken { principal, scopes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::v2_m0::DeviceCapability;
    use crate::v2_m1_northbound::NorthboundPolicyDocument;
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct FakeFetcher {
        calls: Arc<AtomicUsize>,
        sets: Arc<Mutex<Vec<JwkSet>>>,
    }

    #[async_trait]
    impl JwksFetcher for FakeFetcher {
        async fn fetch(&self) -> Result<JwkSet, TokenVerificationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut sets = self.sets.lock().await;
            if sets.is_empty() {
                return Err(TokenVerificationError::Unavailable);
            }
            Ok(sets.remove(0))
        }
    }

    fn config() -> OidcJwtConfig {
        let mut config = OidcJwtConfig::new(
            "https://issuer.example",
            "https://gateway.example/mcp",
            "https://issuer.example/jwks.json",
            vec![OidcJwtAlgorithm::EdDsa],
        );
        config.clock_skew = Duration::ZERO;
        config.unknown_kid_refresh_interval = Duration::from_millis(1);
        config
    }

    fn jwks(signing: &SigningKey, kid: &str) -> JwkSet {
        serde_json::from_value(json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "x": URL_SAFE_NO_PAD.encode(signing.verifying_key().as_bytes()),
                "use": "sig",
                "key_ops": ["verify"],
                "alg": "EdDSA",
                "kid": kid
            }]
        }))
        .unwrap()
    }

    fn token(
        signing: &SigningKey,
        kid: &str,
        issuer: &str,
        audience: &str,
        subject: &str,
        exp_offset_secs: i64,
        nbf_offset_secs: Option<i64>,
    ) -> String {
        let now = jsonwebtoken::get_current_timestamp() as i64;
        let header = json!({"alg":"EdDSA","typ":"JWT","kid":kid});
        let mut claims = json!({
            "iss": issuer,
            "aud": audience,
            "sub": subject,
            "exp": now + exp_offset_secs,
            "scope": "mcp.read mcp.write"
        });
        if let Some(offset) = nbf_offset_secs {
            claims["nbf"] = json!(now + offset);
        }
        let protected = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap());
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let signing_input = format!("{protected}.{payload}");
        let signature = signing.sign(signing_input.as_bytes());
        format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    fn verifier_with_sets(sets: Vec<JwkSet>) -> (OidcJwtVerifier, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let fetcher = FakeFetcher {
            calls: calls.clone(),
            sets: Arc::new(Mutex::new(sets)),
        };
        (
            OidcJwtVerifier::new_with_fetcher(config(), Arc::new(fetcher)).unwrap(),
            calls,
        )
    }

    #[test]
    fn algorithm_allowlist_rejects_symmetric_and_duplicate_configuration() {
        assert!("HS256".parse::<OidcJwtAlgorithm>().is_err());
        assert!("none".parse::<OidcJwtAlgorithm>().is_err());
        let mut duplicate = config();
        duplicate.allowed_algorithms.push(OidcJwtAlgorithm::EdDsa);
        assert!(matches!(
            OidcJwtVerifier::new(duplicate),
            Err(TokenVerificationError::InvalidConfiguration)
        ));
    }

    #[tokio::test]
    async fn valid_signed_token_maps_only_verified_issuer_and_subject() {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let (verifier, _) = verifier_with_sets(vec![jwks(&signing, "key-a")]);
        let token = token(
            &signing,
            "key-a",
            "https://issuer.example",
            "https://gateway.example/mcp",
            "alice",
            300,
            None,
        );
        let verified = verifier.verify(&token).await.unwrap();
        assert_eq!(verified.principal.issuer, "https://issuer.example");
        assert_eq!(verified.principal.subject, "alice");
        assert!(verified.scopes.contains("mcp.read"));
        assert!(verified.scopes.contains("mcp.write"));
    }

    #[tokio::test]
    async fn signature_issuer_audience_time_and_subject_fail_closed() {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let other = SigningKey::generate(&mut rand::rngs::OsRng);
        let (verifier, _) = verifier_with_sets(vec![jwks(&signing, "key-a")]);
        let bad_signature = token(
            &other,
            "key-a",
            "https://issuer.example",
            "https://gateway.example/mcp",
            "alice",
            300,
            None,
        );
        assert!(matches!(
            verifier.verify(&bad_signature).await,
            Err(TokenVerificationError::InvalidToken)
        ));

        for invalid in [
            token(
                &signing,
                "key-a",
                "https://wrong.example",
                "https://gateway.example/mcp",
                "alice",
                300,
                None,
            ),
            token(
                &signing,
                "key-a",
                "https://issuer.example",
                "https://wrong.example/mcp",
                "alice",
                300,
                None,
            ),
            token(
                &signing,
                "key-a",
                "https://issuer.example",
                "https://gateway.example/mcp",
                "alice",
                -1,
                None,
            ),
            token(
                &signing,
                "key-a",
                "https://issuer.example",
                "https://gateway.example/mcp",
                "alice",
                300,
                Some(300),
            ),
            token(
                &signing,
                "key-a",
                "https://issuer.example",
                "https://gateway.example/mcp",
                "   ",
                300,
                None,
            ),
        ] {
            assert!(matches!(
                verifier.verify(&invalid).await,
                Err(TokenVerificationError::InvalidToken)
            ));
        }
    }

    #[tokio::test]
    async fn unknown_kid_refresh_is_bounded_and_rotation_can_succeed() {
        let first = SigningKey::generate(&mut rand::rngs::OsRng);
        let rotated = SigningKey::generate(&mut rand::rngs::OsRng);
        let (verifier, calls) =
            verifier_with_sets(vec![jwks(&first, "old"), jwks(&rotated, "new")]);
        let old = token(
            &first,
            "old",
            "https://issuer.example",
            "https://gateway.example/mcp",
            "alice",
            300,
            None,
        );
        verifier.verify(&old).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2)).await;
        let new = token(
            &rotated,
            "new",
            "https://issuer.example",
            "https://gateway.example/mcp",
            "alice",
            300,
            None,
        );
        verifier.verify(&new).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn two_verified_subjects_remain_distinct_at_exact_authorizer_boundary() {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let (verifier, _) = verifier_with_sets(vec![jwks(&signing, "key-a")]);
        let alice = verifier
            .verify(&token(
                &signing,
                "key-a",
                "https://issuer.example",
                "https://gateway.example/mcp",
                "alice",
                300,
                None,
            ))
            .await
            .unwrap();
        let bob = verifier
            .verify(&token(
                &signing,
                "key-a",
                "https://issuer.example",
                "https://gateway.example/mcp",
                "bob",
                300,
                None,
            ))
            .await
            .unwrap();

        let policy = NorthboundPolicyDocument::from_json(
            r#"{"grants":[
                {"issuer":"https://issuer.example","subject":"alice","device_id":"mac","capabilities":["read_file"]},
                {"issuer":"https://issuer.example","subject":"bob","device_id":"mac","capabilities":["shell"]}
            ]}"#,
        )
        .unwrap()
        .build_policy("https://issuer.example", "mac")
        .unwrap();

        assert!(
            policy
                .authorize_device_capability(&alice.principal, "mac", DeviceCapability::ReadFile)
                .is_ok()
        );
        assert!(
            policy
                .authorize_device_capability(&alice.principal, "mac", DeviceCapability::Shell)
                .is_err()
        );
        assert!(
            policy
                .authorize_device_capability(&bob.principal, "mac", DeviceCapability::Shell)
                .is_ok()
        );
        assert!(
            policy
                .authorize_device_capability(&bob.principal, "mac", DeviceCapability::ReadFile)
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_kid_and_algorithm_confusion_fail_before_authorization() {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let (verifier, _) = verifier_with_sets(vec![jwks(&signing, "key-a")]);
        let now = jsonwebtoken::get_current_timestamp();
        let claims = json!({
            "iss":"https://issuer.example",
            "aud":"https://gateway.example/mcp",
            "sub":"alice",
            "exp":now + 300
        });
        let header = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"alg":"EdDSA"})).unwrap());
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let input = format!("{header}.{payload}");
        let missing_kid = format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signing.sign(input.as_bytes()).to_bytes())
        );
        assert!(matches!(
            verifier.verify(&missing_kid).await,
            Err(TokenVerificationError::InvalidToken)
        ));

        let header = URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"alg":"HS256","kid":"key-a"})).unwrap());
        let input = format!("{header}.{payload}");
        let confused = format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(signing.sign(input.as_bytes()).to_bytes())
        );
        assert!(matches!(
            verifier.verify(&confused).await,
            Err(TokenVerificationError::InvalidToken)
        ));
    }

    #[tokio::test]
    async fn repeated_unknown_kid_does_not_trigger_unbounded_jwks_fetches() {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let unknown = SigningKey::generate(&mut rand::rngs::OsRng);
        let (verifier, calls) = verifier_with_sets(vec![jwks(&signing, "known")]);
        let token = token(
            &unknown,
            "unknown",
            "https://issuer.example",
            "https://gateway.example/mcp",
            "alice",
            300,
            None,
        );
        assert!(matches!(
            verifier.verify(&token).await,
            Err(TokenVerificationError::InvalidToken)
        ));
        assert!(matches!(
            verifier.verify(&token).await,
            Err(TokenVerificationError::InvalidToken)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn issuer_jwks_and_generic_audience_configuration_are_bounded() {
        let mut bad_issuer = config();
        bad_issuer.issuer = "https://issuer.example?tenant=1".into();
        assert!(matches!(
            OidcJwtVerifier::new(bad_issuer),
            Err(TokenVerificationError::InvalidConfiguration)
        ));
        let mut opaque_audience = config();
        opaque_audience.audience = "api-client-id-123".into();
        assert!(OidcJwtVerifier::new(opaque_audience).is_ok());
        let mut bad_audience = config();
        bad_audience.audience = "bad\naudience".into();
        assert!(matches!(
            OidcJwtVerifier::new(bad_audience),
            Err(TokenVerificationError::InvalidConfiguration)
        ));
        let mut jwks_query = config();
        jwks_query.jwks_uri = "https://issuer.example/jwks?version=1".into();
        assert!(OidcJwtVerifier::new(jwks_query).is_ok());
    }

    #[tokio::test]
    async fn jwk_usage_and_algorithm_mismatch_are_rejected() {
        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let mut bad_use = serde_json::to_value(jwks(&signing, "key-a")).unwrap();
        bad_use["keys"][0]["use"] = json!("enc");
        let set: JwkSet = serde_json::from_value(bad_use).unwrap();
        let (verifier, _) = verifier_with_sets(vec![set]);
        let token = token(
            &signing,
            "key-a",
            "https://issuer.example",
            "https://gateway.example/mcp",
            "alice",
            300,
            None,
        );
        assert!(matches!(
            verifier.verify(&token).await,
            Err(TokenVerificationError::InvalidToken)
        ));

        let mut bad_alg = serde_json::to_value(jwks(&signing, "key-a")).unwrap();
        bad_alg["keys"][0]["alg"] = json!("RS256");
        let set: JwkSet = serde_json::from_value(bad_alg).unwrap();
        let (verifier, _) = verifier_with_sets(vec![set]);
        assert!(matches!(
            verifier.verify(&token).await,
            Err(TokenVerificationError::InvalidToken)
        ));
    }
}
