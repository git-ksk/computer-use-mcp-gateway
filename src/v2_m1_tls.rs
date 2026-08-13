//! V2-M1 encrypted Hub↔Agent transport wrapper.
//!
//! TLS provides confidentiality and channel integrity. The V2 application-layer
//! Ed25519 identities/signatures remain in place because they bind logical Hub
//! and Agent identities, grants, generations, and operation messages independent
//! of the underlying transport implementation.

use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, RootCertStore, ServerConfig, ServerConnection, StreamOwned,
};
use std::fmt;
use std::net::TcpStream;
use std::sync::Arc;

pub const HUB_AGENT_ALPN: &[u8] = b"cumg-hub-agent/1";

pub fn server_config_from_der(
    certificate_chain_der: Vec<Vec<u8>>,
    private_key_pkcs8_der: Vec<u8>,
) -> Result<Arc<ServerConfig>, TlsTransportError> {
    if certificate_chain_der.is_empty() {
        return Err(TlsTransportError::EmptyCertificateChain);
    }
    let certs = certificate_chain_der
        .into_iter()
        .map(CertificateDer::from)
        .collect();
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(private_key_pkcs8_der));
    let mut config = ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(TlsTransportError::Tls)?;
    config.alpn_protocols = vec![HUB_AGENT_ALPN.to_vec()];
    Ok(Arc::new(config))
}

pub fn client_config_with_pinned_root(
    root_certificate_der: Vec<u8>,
) -> Result<Arc<ClientConfig>, TlsTransportError> {
    let mut roots = RootCertStore::empty();
    roots
        .add(CertificateDer::from(root_certificate_der))
        .map_err(TlsTransportError::Tls)?;
    let mut config = ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![HUB_AGENT_ALPN.to_vec()];
    Ok(Arc::new(config))
}

pub type HubTlsStream = StreamOwned<ServerConnection, TcpStream>;
pub type AgentTlsStream = StreamOwned<ClientConnection, TcpStream>;

pub fn accept_hub_tls(
    stream: TcpStream,
    config: Arc<ServerConfig>,
) -> Result<HubTlsStream, TlsTransportError> {
    let connection = ServerConnection::new(config).map_err(TlsTransportError::Tls)?;
    let mut tls = StreamOwned::new(connection, stream);
    complete_server_handshake(&mut tls)?;
    ensure_alpn(tls.conn.alpn_protocol())?;
    Ok(tls)
}

pub fn connect_agent_tls(
    stream: TcpStream,
    server_name: &str,
    config: Arc<ClientConfig>,
) -> Result<AgentTlsStream, TlsTransportError> {
    let server_name = ServerName::try_from(server_name.to_owned())
        .map_err(|_| TlsTransportError::InvalidServerName)?;
    let connection = ClientConnection::new(config, server_name).map_err(TlsTransportError::Tls)?;
    let mut tls = StreamOwned::new(connection, stream);
    complete_client_handshake(&mut tls)?;
    ensure_alpn(tls.conn.alpn_protocol())?;
    Ok(tls)
}

fn complete_server_handshake(stream: &mut HubTlsStream) -> Result<(), TlsTransportError> {
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(TlsTransportError::Io)?;
    }
    Ok(())
}

fn complete_client_handshake(stream: &mut AgentTlsStream) -> Result<(), TlsTransportError> {
    while stream.conn.is_handshaking() {
        stream
            .conn
            .complete_io(&mut stream.sock)
            .map_err(TlsTransportError::Io)?;
    }
    Ok(())
}

fn ensure_alpn(negotiated: Option<&[u8]>) -> Result<(), TlsTransportError> {
    if negotiated == Some(HUB_AGENT_ALPN) {
        Ok(())
    } else {
        Err(TlsTransportError::AlpnMismatch)
    }
}

#[derive(Debug)]
pub enum TlsTransportError {
    Io(std::io::Error),
    Tls(rustls::Error),
    EmptyCertificateChain,
    InvalidServerName,
    AlpnMismatch,
}

impl fmt::Display for TlsTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "TLS I/O error: {error}"),
            Self::Tls(error) => write!(f, "TLS protocol error: {error}"),
            Self::EmptyCertificateChain => write!(f, "TLS certificate chain is empty"),
            Self::InvalidServerName => write!(f, "TLS server name is invalid"),
            Self::AlpnMismatch => write!(f, "TLS ALPN did not negotiate cumg-hub-agent/1"),
        }
    }
}

impl std::error::Error for TlsTransportError {}

#[cfg(test)]
mod tests {
    use super::*;
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn test_material() -> (Vec<u8>, Vec<u8>) {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        (cert.der().to_vec(), signing_key.serialize_der())
    }

    #[test]
    fn tls13_with_pinned_certificate_and_alpn_round_trips() {
        let (cert, key) = test_material();
        let server_config = server_config_from_der(vec![cert.clone()], key).unwrap();
        let client_config = client_config_with_pinned_root(cert).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut tls = accept_hub_tls(stream, server_config).unwrap();
            let mut input = [0_u8; 4];
            tls.read_exact(&mut input).unwrap();
            assert_eq!(&input, b"ping");
            tls.write_all(b"pong").unwrap();
            tls.flush().unwrap();
            assert_eq!(
                tls.conn.protocol_version(),
                Some(rustls::ProtocolVersion::TLSv1_3)
            );
        });

        let stream = TcpStream::connect(address).unwrap();
        let mut tls = connect_agent_tls(stream, "localhost", client_config).unwrap();
        tls.write_all(b"ping").unwrap();
        tls.flush().unwrap();
        let mut output = [0_u8; 4];
        tls.read_exact(&mut output).unwrap();
        assert_eq!(&output, b"pong");
        assert_eq!(
            tls.conn.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_3)
        );
        server.join().unwrap();
    }

    #[test]
    fn untrusted_server_certificate_fails_closed() {
        let (server_cert, server_key) = test_material();
        let (wrong_root, _) = test_material();
        let server_config = server_config_from_der(vec![server_cert], server_key).unwrap();
        let client_config = client_config_with_pinned_root(wrong_root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = accept_hub_tls(stream, server_config);
        });
        let stream = TcpStream::connect(address).unwrap();
        assert!(connect_agent_tls(stream, "localhost", client_config).is_err());
        server.join().unwrap();
    }
}
