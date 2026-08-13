// Owns rustls listener configuration and development certificate generation.
// config.rs supplies certificate paths; keys.rs separately owns the application identity key.

// TLS (Hive_design_doc.md §2.1): HTTPS only, no plaintext listener even in
// dev. Prod uses real PEM cert/key from config; dev auto-generates a
// self-signed cert cached in the data dir.

use crate::config::Config;
use anyhow::Result;
use axum_server::tls_rustls::RustlsConfig;
use std::fs;

pub async fn rustls_config(cfg: &Config) -> Result<RustlsConfig> {
    if let (Some(cert), Some(key)) = (&cfg.tls_cert, &cfg.tls_key) {
        return Ok(RustlsConfig::from_pem_file(cert, key).await?);
    }
    anyhow::ensure!(
        cfg.dev,
        "production requires tls_cert and tls_key in hive.toml (no plaintext listener exists)"
    );
    let cert_path = cfg.data_dir.join("dev_cert.pem");
    let key_path = cfg.data_dir.join("dev_key.pem");
    if !cert_path.exists() || !key_path.exists() {
        let ck = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])?;
        fs::write(&cert_path, ck.cert.pem())?;
        fs::write(&key_path, ck.key_pair.serialize_pem())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
        }
        tracing::info!("generated self-signed dev TLS certificate");
    }
    Ok(RustlsConfig::from_pem_file(cert_path, key_path).await?)
}
