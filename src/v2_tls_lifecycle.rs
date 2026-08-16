use std::fmt;
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use x509_parser::parse_x509_certificate;
use x509_parser::pem::parse_x509_pem;

const MAX_CERTIFICATE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateFormat {
    Pem,
    Der,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateHealth {
    Healthy,
    Expiring,
    Expired,
    NotYetValid,
}

impl CertificateHealth {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Expiring => "expiring",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CertificateInspection {
    pub health: CertificateHealth,
    pub not_before_unix_secs: i64,
    pub not_after_unix_secs: i64,
    pub remaining_secs: i64,
}

pub fn inspect_certificate_file(
    path: &Path,
    format: CertificateFormat,
    warn_before_secs: u64,
) -> Result<CertificateInspection, TlsLifecycleError> {
    let metadata = std::fs::symlink_metadata(path).map_err(TlsLifecycleError::Io)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TlsLifecycleError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(TlsLifecycleError::WritableTrustMaterial);
        }
    }
    if metadata.len() == 0 || metadata.len() > MAX_CERTIFICATE_BYTES {
        return Err(TlsLifecycleError::InvalidSize);
    }
    let file = File::open(path).map_err(TlsLifecycleError::Io)?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_CERTIFICATE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(TlsLifecycleError::Io)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CERTIFICATE_BYTES {
        return Err(TlsLifecycleError::InvalidSize);
    }
    let now = current_unix_secs()?;
    inspect_certificate_bytes(&bytes, format, warn_before_secs, now)
}

pub fn inspect_certificate_bytes(
    bytes: &[u8],
    format: CertificateFormat,
    warn_before_secs: u64,
    now_unix_secs: i64,
) -> Result<CertificateInspection, TlsLifecycleError> {
    let (not_before, not_after) = certificate_validity(bytes, format)?;
    let remaining = not_after.saturating_sub(now_unix_secs);
    let health = if now_unix_secs < not_before {
        CertificateHealth::NotYetValid
    } else if now_unix_secs >= not_after {
        CertificateHealth::Expired
    } else if remaining <= i64::try_from(warn_before_secs).unwrap_or(i64::MAX) {
        CertificateHealth::Expiring
    } else {
        CertificateHealth::Healthy
    };
    Ok(CertificateInspection {
        health,
        not_before_unix_secs: not_before,
        not_after_unix_secs: not_after,
        remaining_secs: remaining,
    })
}

fn certificate_validity(
    bytes: &[u8],
    format: CertificateFormat,
) -> Result<(i64, i64), TlsLifecycleError> {
    if bytes.is_empty() {
        return Err(TlsLifecycleError::InvalidCertificate);
    }
    match format {
        CertificateFormat::Der => {
            let (remainder, certificate) =
                parse_x509_certificate(bytes).map_err(|_| TlsLifecycleError::InvalidCertificate)?;
            if !remainder.is_empty() {
                return Err(TlsLifecycleError::InvalidCertificate);
            }
            Ok((
                certificate.validity().not_before.timestamp(),
                certificate.validity().not_after.timestamp(),
            ))
        }
        CertificateFormat::Pem => {
            let (_, pem) =
                parse_x509_pem(bytes).map_err(|_| TlsLifecycleError::InvalidCertificate)?;
            if pem.label != "CERTIFICATE" {
                return Err(TlsLifecycleError::InvalidCertificate);
            }
            let certificate = pem
                .parse_x509()
                .map_err(|_| TlsLifecycleError::InvalidCertificate)?;
            Ok((
                certificate.validity().not_before.timestamp(),
                certificate.validity().not_after.timestamp(),
            ))
        }
    }
}

fn current_unix_secs() -> Result<i64, TlsLifecycleError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| TlsLifecycleError::SystemClockBeforeEpoch)?;
    i64::try_from(duration.as_secs()).map_err(|_| TlsLifecycleError::SystemClockOutOfRange)
}

#[derive(Debug)]
pub enum TlsLifecycleError {
    Io(std::io::Error),
    UnsafePath,
    WritableTrustMaterial,
    InvalidSize,
    InvalidCertificate,
    SystemClockBeforeEpoch,
    SystemClockOutOfRange,
}

impl TlsLifecycleError {
    pub const fn safe_error_code(&self) -> &'static str {
        match self {
            Self::Io(_) => "certificate_io_error",
            Self::UnsafePath => "unsafe_certificate_path",
            Self::WritableTrustMaterial => "writable_trust_material",
            Self::InvalidSize => "invalid_certificate_size",
            Self::InvalidCertificate => "invalid_certificate",
            Self::SystemClockBeforeEpoch | Self::SystemClockOutOfRange => "invalid_system_clock",
        }
    }
}

impl fmt::Display for TlsLifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.safe_error_code())
    }
}

impl std::error::Error for TlsLifecycleError {}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertifiedKey, generate_simple_self_signed};

    #[test]
    fn certificate_health_has_deterministic_thresholds_for_der_and_pem() {
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let der = cert.der().to_vec();
        let pem = cert.pem().into_bytes();
        let (not_before, not_after) = certificate_validity(&der, CertificateFormat::Der).unwrap();
        assert!(not_after > not_before + 60);

        let before =
            inspect_certificate_bytes(&der, CertificateFormat::Der, 30, not_before - 1).unwrap();
        assert_eq!(before.health, CertificateHealth::NotYetValid);

        let healthy =
            inspect_certificate_bytes(&pem, CertificateFormat::Pem, 30, not_before + 1).unwrap();
        assert_eq!(healthy.health, CertificateHealth::Healthy);

        let expiring =
            inspect_certificate_bytes(&der, CertificateFormat::Der, 30, not_after - 20).unwrap();
        assert_eq!(expiring.health, CertificateHealth::Expiring);
        assert_eq!(expiring.remaining_secs, 20);

        let expired =
            inspect_certificate_bytes(&pem, CertificateFormat::Pem, 30, not_after).unwrap();
        assert_eq!(expired.health, CertificateHealth::Expired);
        assert_eq!(expired.remaining_secs, 0);
    }

    #[test]
    fn malformed_or_trailing_der_fails_closed() {
        assert!(matches!(
            inspect_certificate_bytes(&[1, 2, 3], CertificateFormat::Der, 30, 0),
            Err(TlsLifecycleError::InvalidCertificate)
        ));
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let mut der = cert.der().to_vec();
        der.push(0);
        assert!(matches!(
            inspect_certificate_bytes(&der, CertificateFormat::Der, 30, 0),
            Err(TlsLifecycleError::InvalidCertificate)
        ));
    }
}
