//! Mutual TLS identity pins and pairing-only bootstrap. TLS handshake signatures
//! are always verified, including before the user confirms the pairing code.
use anyhow::Result;
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime},
    server::danger::{ClientCertVerified, ClientCertVerifier},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Instant,
};
#[derive(Debug, Default)]
pub struct Gate {
    pub enabled: bool,
    pub pins: BTreeSet<String>,
    pub pairing_until: Option<Instant>,
}
#[derive(Debug)]
struct Verifier(Arc<Mutex<Gate>>);
pub fn fingerprint(cert: &[u8]) -> String {
    Sha256::digest(cert)
        .iter()
        .map(|v| format!("{v:02x}"))
        .collect()
}
impl Verifier {
    fn check(&self, cert: &CertificateDer<'_>) -> std::result::Result<(), rustls::Error> {
        let gate = self.0.lock().unwrap();
        if cert.len() > 16384
            || !gate.enabled
            || !(gate.pins.contains(&fingerprint(cert))
                || gate.pairing_until.is_some_and(|t| t > Instant::now()))
        {
            return Err(rustls::Error::General(
                "Peer is not paired; open pairing explicitly on both computers".into(),
            ));
        }
        Ok(())
    }
    fn sig12(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            m,
            c,
            s,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn sig13(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            m,
            c,
            s,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        c: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        self.check(c)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.sig12(m, c, s)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.sig13(m, c, s)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}
impl ClientCertVerifier for Verifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }
    fn verify_client_cert(
        &self,
        c: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        self.check(c)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.sig12(m, c, s)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        s: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.sig13(m, c, s)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}
#[derive(Serialize, Deserialize)]
pub struct Identity {
    pub certificate: Vec<u8>,
    key: Vec<u8>,
}
impl Identity {
    pub fn load(root: &std::path::Path) -> Result<Self> {
        let path = root.join("peer-identity.json");
        if path.exists() {
            return Ok(serde_json::from_slice(&std::fs::read(path)?)?);
        }
        let cert = rcgen::generate_simple_self_signed(vec!["easy-switch-link.local".into()])?;
        let id = Self {
            certificate: cert.cert.der().to_vec(),
            key: cert.signing_key.serialize_der(),
        };
        crate::storage::write_json(&path, &id)?;
        Ok(id)
    }
    pub fn configs(
        &self,
        gate: Arc<Mutex<Gate>>,
    ) -> Result<(Arc<rustls::ClientConfig>, Arc<rustls::ServerConfig>)> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let key = || PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()));
        let certs = || vec![CertificateDer::from(self.certificate.clone())];
        let mut client = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Verifier(gate.clone())))
            .with_client_auth_cert(certs(), key())?;
        let mut server = rustls::ServerConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(Arc::new(Verifier(gate)))
            .with_single_cert(certs(), key())?;
        client.alpn_protocols = vec![b"eslink/1".to_vec()];
        server.alpn_protocols = client.alpn_protocols.clone();
        Ok((Arc::new(client), Arc::new(server)))
    }
}
/// Both peers hash the same ordered identities and fresh nonces. Never treat the
/// displayed six digits as a password or transmit them as authentication proof.
pub fn pairing_code(a: &str, an: &str, b: &str, bn: &str) -> String {
    let parts = if a < b {
        [a, an, b, bn]
    } else {
        [b, bn, a, an]
    };
    let mut h = Sha256::new();
    for p in parts {
        h.update((p.len() as u32).to_be_bytes());
        h.update(p.as_bytes());
    }
    let d = h.finalize();
    format!(
        "{:06}",
        u32::from_be_bytes(d[..4].try_into().unwrap()) % 1_000_000
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mutual_tls_requires_pairing_or_pins_and_revocation_takes_effect() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let a_dir = tempfile::tempdir().unwrap();
            let b_dir = tempfile::tempdir().unwrap();
            let a = Identity::load(a_dir.path()).unwrap();
            let b = Identity::load(b_dir.path()).unwrap();
            let ga = Arc::new(Mutex::new(Gate {
                enabled: true,
                ..Default::default()
            }));
            let gb = Arc::new(Mutex::new(Gate {
                enabled: true,
                ..Default::default()
            }));
            let (ac, _) = a.configs(ga.clone()).unwrap();
            let (_, bs) = b.configs(gb.clone()).unwrap();
            async fn handshake(
                ac: Arc<rustls::ClientConfig>,
                bs: Arc<rustls::ServerConfig>,
            ) -> bool {
                let (a, b) = tokio::io::duplex(65536);
                let client = tokio_rustls::TlsConnector::from(ac);
                let server = tokio_rustls::TlsAcceptor::from(bs);
                let (c, s) = tokio::join!(
                    client.connect(
                        rustls::pki_types::ServerName::try_from("easy-switch-link.local").unwrap(),
                        a
                    ),
                    server.accept(b)
                );
                c.is_ok() && s.is_ok()
            }
            assert!(!handshake(ac.clone(), bs.clone()).await);
            ga.lock().unwrap().pairing_until =
                Some(Instant::now() + std::time::Duration::from_secs(5));
            gb.lock().unwrap().pairing_until = ga.lock().unwrap().pairing_until;
            assert!(handshake(ac.clone(), bs.clone()).await);
            ga.lock().unwrap().pairing_until = None;
            gb.lock().unwrap().pairing_until = None;
            ga.lock().unwrap().pins.insert(fingerprint(&b.certificate));
            gb.lock().unwrap().pins.insert(fingerprint(&a.certificate));
            assert!(handshake(ac.clone(), bs.clone()).await);
            gb.lock().unwrap().pins.clear();
            assert!(!handshake(ac, bs).await);
        });
    }
    #[test]
    fn pairing_binds_both_identities_and_nonces() {
        assert_eq!(
            pairing_code("a", "one", "b", "two"),
            pairing_code("b", "two", "a", "one")
        );
        assert_ne!(
            pairing_code("a", "one", "b", "two"),
            pairing_code("a", "other", "b", "two")
        );
    }
    #[test]
    fn closed_pairing_rejects_unpinned_certificates() {
        let gate = Arc::new(Mutex::new(Gate {
            enabled: true,
            ..Default::default()
        }));
        let v = Verifier(gate.clone());
        let c = CertificateDer::from(vec![1, 2, 3]);
        assert!(v.check(&c).is_err());
        gate.lock().unwrap().pins.insert(fingerprint(&c));
        assert!(v.check(&c).is_ok());
        gate.lock().unwrap().enabled = false;
        assert!(v.check(&c).is_err());
    }
}
