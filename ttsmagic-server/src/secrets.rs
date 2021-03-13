use anyhow::{Context, Result};
use serde::Deserialize;
use std::{fs::File, io::Read as _, path::Path, sync::RwLock};
use ttsmagic_s3::S3Credentials;

#[derive(Clone, Debug, Deserialize)]
pub struct LinodeObjectStorageSecrets {
    pub access_key: String,
    pub secret_key: String,
}

impl Into<S3Credentials> for LinodeObjectStorageSecrets {
    fn into(self) -> S3Credentials {
        S3Credentials {
            access_key: self.access_key,
            secret_key: self.secret_key,
        }
    }
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

    fn linode_credentials(&self) -> LinodeObjectStorageSecrets {
        self.linode_object_storage.clone()
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

#[inline]
fn get_secret<F, T>(f: F) -> T
where
    F: Fn(&Secrets) -> T,
{
    let l = SECRETS.read().unwrap();
    match l.as_ref() {
        Some(s) => f(s),
        None => {
            if cfg!(test) {
                drop(l);
                let cwd = std::env::current_dir().unwrap();
                let mut dir: &Path = &cwd;
                let mut toml_path;
                if let Some(path_str) = option_env!("SECRETS_TOML") {
                    // CARGO_MANIFEST_DIR is the path to (and including) `ttsmagic-server`.
                    toml_path = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
                    // By default, this path is relative to the workspace root, so go up a level.
                    toml_path.push("..");
                    toml_path.push(path_str);
                } else {
                    loop {
                        toml_path = dir.join("secrets.toml");
                        if toml_path.is_file() {
                            break;
                        }
                        dir = dir.parent().expect("Failed to find secrets.toml file");
                    }
                };
                toml_path = toml_path.canonicalize().unwrap();
                println!("Loading secrets from {:?}", toml_path.to_string_lossy());
                init_from_toml(toml_path).unwrap();
                let l = SECRETS.read().unwrap();
                let secrets = l.as_ref().unwrap();
                f(secrets)
            } else {
                panic!(
                    "Attempted to access secret {:?} before secrets were initialized!",
                    stringify!($name)
                )
            }
        }
    }
}

macro_rules! secret_access {
    ($name:ident -> $t:ty) => {
        pub fn $name() -> $t {
            get_secret(|s| s.$name())
        }
    };
}

secret_access!(steam_api_key -> String);
secret_access!(session_private_key -> [u8; 32]);
secret_access!(linode_credentials -> LinodeObjectStorageSecrets);
