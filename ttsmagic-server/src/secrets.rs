use anyhow::{Context, Result};
use rusoto_credential::StaticProvider;
use serde::Deserialize;
use std::{fs::File, io::Read as _, path::Path, sync::RwLock};

#[derive(Debug, Deserialize)]
struct LinodeObjectStorageSecrets {
    access_key: String,
    secret_key: String,
}

#[derive(Debug, Deserialize)]
struct Secrets {
    linode_object_storage: LinodeObjectStorageSecrets,
    steam_api_key: String,
    session_private_key_hex: String,
}

impl Secrets {
    fn steam_api_key(&self) -> String {
        self.steam_api_key.clone()
    }

    fn session_private_key(&self) -> [u8; 32] {
        assert_eq!(self.session_private_key_hex.len(), 32 * 2);
        let mut decoded_bytes = [0; 32];
        let hex_bytes = self.session_private_key_hex.as_bytes();
        hex::decode_to_slice(hex_bytes, &mut decoded_bytes).unwrap();
        decoded_bytes
    }

    fn linode_credentials(&self) -> StaticProvider {
        StaticProvider::new(
            self.linode_object_storage.access_key.clone(),
            self.linode_object_storage.secret_key.clone(),
            None,
            None,
        )
    }
}

lazy_static::lazy_static! {
    static ref SECRETS: RwLock<Option<Secrets>> = RwLock::new(None);
}

pub fn init_from_toml<P: AsRef<Path>>(path: P) -> Result<()> {
    let mut f = File::open(path).context("Failed to open secrets TOML file")?;
    let mut buffer = Vec::with_capacity(1024);
    f.read_to_end(&mut buffer)?;
    let secrets: Secrets =
        toml::from_slice(buffer.as_slice()).context("Failed to parse secrets TOML file")?;
    let mut l = SECRETS.write().unwrap();
    *l = Some(secrets);
    Ok(())
}

macro_rules! secret_access {
    ($name:ident -> $t:ty) => {
        pub fn $name() -> $t {
            let l = SECRETS.read().unwrap();
            let secrets_opt: Option<&Secrets> = l.as_ref();
            match secrets_opt {
                Some(s) => s.$name(),
                None => panic!(
                    "Attempted to access secret {:?} before secrets were initialized!",
                    stringify!($name)
                ),
            }
        }
    };
}

secret_access!(steam_api_key -> String);
secret_access!(session_private_key -> [u8; 32]);
secret_access!(linode_credentials -> StaticProvider);
