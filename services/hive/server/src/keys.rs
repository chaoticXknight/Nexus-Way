// Owns creation and durable loading of HIVE's Ed25519 server identity key.
// lib.rs publishes the public identity; TLS transport certificates remain owned by tls.rs.

// Server identity key (Hive_design_doc.md §2.3): an Ed25519 keypair generated
// at first boot, stored in the data dir so host migration (dev box → prod box)
// is a copy, not a re-trust. Clients TOFU-pin the public key.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use std::fs;
use std::path::Path;

pub fn load_or_create(data_dir: &Path) -> Result<SigningKey> {
    let path = data_dir.join("server_key");
    if path.exists() {
        let bytes = fs::read(&path).context("reading server_key")?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .context("server_key must be exactly 32 bytes")?;
        Ok(SigningKey::from_bytes(&seed))
    } else {
        let key = SigningKey::generate(&mut rand::rngs::OsRng);
        fs::write(&path, key.to_bytes()).context("writing server_key")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        }
        tracing::info!("generated new server identity key");
        Ok(key)
    }
}
