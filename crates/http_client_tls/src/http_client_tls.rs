use std::sync::OnceLock;

use rustls::ClientConfig;
use rustls_platform_verifier::ConfigVerifierExt;

static TLS_CONFIG: OnceLock<rustls::ClientConfig> = OnceLock::new();

pub fn tls_config() -> ClientConfig {
    TLS_CONFIG
        .get_or_init(|| {
            // aws-lc aborts inside its AES key schedule on OHOS, where its own
            // CI has never run it, so the ring provider is chosen instead. Both
            // are compiled in through other dependencies, which is why the
            // provider has to be explicit; installing only fails when another
            // provider is already in place, and that is ignored.
            rustls::crypto::ring::default_provider()
                .install_default()
                .ok();

            ClientConfig::with_platform_verifier().unwrap_or_else(|error| {
                log::error!(
                    "failed to load platform TLS certificate verifier, falling back to bundled webpki roots: {error}"
                );
                ClientConfig::builder()
                    .with_root_certificates(rustls::RootCertStore {
                        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
                    })
                    .with_no_client_auth()
            })
        })
        .clone()
}
