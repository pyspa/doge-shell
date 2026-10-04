//! Owner-only SIWC registration and credential storage. Never uses shell sessions.
use super::*;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct AuthStore {
    pub(crate) root: PathBuf,
    #[cfg(test)]
    pub(crate) token_endpoint: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Registrations {
    pub host_id: String,
    pub active: Option<String>,
    pub accounts: Vec<Account>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Account {
    pub label: String,
    pub client_id: String,
    pub subject: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Credentials {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub scopes: Vec<String>,
    pub expires_at: u64,
    pub earliest_refresh_at: u64,
}
impl Credentials {
    pub fn check_scopes(&self) -> Result<()> {
        for scope in [
            "resource.invoke",
            "chatgpt.tokens.use.direct",
            "offline_access",
        ] {
            if !self.scopes.iter().any(|s| s == scope) {
                bail!(
                    "ChatGPT plan permission is missing; run chat_auth login. No API-key fallback is used."
                );
            }
        }
        if self.access_token.is_empty() || self.refresh_token.is_empty() {
            bail!("ChatGPT credentials are incomplete; run chat_auth login.");
        }
        Ok(())
    }
}
impl std::fmt::Debug for AuthStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthStore([private])")
    }
}
impl AuthStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            #[cfg(test)]
            token_endpoint: None,
        }
    }
    pub(crate) fn token_endpoint(&self) -> &str {
        #[cfg(test)]
        if let Some(endpoint) = &self.token_endpoint {
            return endpoint;
        }
        super::TOKEN_ENDPOINT
    }
    fn check_path(&self, path: &Path) -> Result<()> {
        if !path.is_absolute() {
            bail!("Authentication storage requires an absolute path.");
        }
        for ancestor in path.ancestors() {
            match fs::symlink_metadata(ancestor) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    bail!("Authentication storage refuses symlinks.")
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => bail!("Cannot inspect authentication storage."),
            }
        }
        Ok(())
    }
    fn check_private(&self, path: &Path, directory: bool) -> Result<()> {
        self.check_path(path)?;
        let meta = fs::symlink_metadata(path)
            .map_err(|_| anyhow!("Authentication storage is unavailable; run chat_auth login."))?;
        // effective uid is the owner of files created by this process on both supported OSes.
        if meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || (directory && !meta.is_dir())
            || (!directory && !meta.is_file())
            || (!directory && meta.nlink() != 1)
        {
            bail!("Authentication storage must be owner-only (directory 0700, files 0600).");
        }
        Ok(())
    }
    pub(crate) fn init(&self) -> Result<()> {
        self.check_path(&self.root)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)
            .map_err(|_| anyhow!("Cannot create private authentication directory."))?;
        self.check_private(&self.root, true)
    }
    fn open(&self, name: &str, create: bool) -> Result<File> {
        self.check_private(&self.root, true)?;
        let path = self.root.join(name);
        self.check_path(&path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(create)
            .create(create)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|_| anyhow!("Authentication record is unavailable; run chat_auth login."))?;
        self.check_private(&path, false)?;
        Ok(file)
    }
    pub(crate) async fn lock(&self, cancel: Option<&dyn Fn() -> bool>) -> Result<File> {
        self.init()?;
        let file = self.open("session.lock", true)?;
        wait(
            async {
                loop {
                    match file.try_lock() {
                        Ok(()) => return Ok(file),
                        Err(std::fs::TryLockError::WouldBlock) => {
                            tokio::time::sleep(Duration::from_millis(50)).await
                        }
                        Err(_) => bail!("Cannot lock authentication storage."),
                    }
                }
            },
            cancel,
            Duration::from_secs(30),
        )
        .await
    }
    fn read<T: serde::de::DeserializeOwned>(&self, name: &str) -> Result<T> {
        let mut bytes = Vec::new();
        self.open(name, false)?
            .take(1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("Cannot read authentication record."))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("Authentication record is invalid; run chat_auth login."))
    }
    pub(crate) fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        self.check_private(&self.root, true)?;
        let path = self.root.join(name);
        self.check_path(&path)?;
        if path.exists() {
            self.check_private(&path, false)?;
        }
        let temporary = self
            .root
            .join(format!(".credentials-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|_| anyhow!("Cannot create authentication record."))?;
        let bytes = serde_json::to_vec(value)
            .map_err(|_| anyhow!("Cannot encode authentication record."))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| anyhow!("Cannot save authentication record."))?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        self.check_path(&path)?;
        fs::rename(&temporary, &path)
            .map_err(|_| anyhow!("Cannot replace authentication record."))?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    pub(crate) fn pending_id(&self) -> Result<Option<String>> {
        let path = self.root.join("pending-registration.json");
        self.check_path(&path)?;
        if !path.exists() {
            return Ok(None);
        }
        let id: String = self.read("pending-registration.json")?;
        if id.is_empty() || id == "dynamic_agent_client" {
            bail!("Invalid pending registration.");
        }
        Ok(Some(id))
    }
    pub(crate) fn clear_pending(&self) -> Result<()> {
        let path = self.root.join("pending-registration.json");
        self.check_path(&path)?;
        if path.exists() {
            self.check_private(&path, false)?;
            fs::remove_file(path)?;
        }
        Ok(())
    }
    pub(crate) fn registrations(&self) -> Result<Registrations> {
        self.read("registration.json")
    }
    pub(crate) fn registration_or_new(&self) -> Result<Registrations> {
        self.check_path(&self.root.join("registration.json"))?;
        if !self.root.join("registration.json").exists() {
            let reg = Registrations {
                host_id: format!("urn:uuid:{}", uuid::Uuid::new_v4()),
                ..Default::default()
            };
            self.save("registration.json", &reg)?;
            Ok(reg)
        } else {
            self.registrations()
        }
    }
    pub(crate) fn credential_name(label: &str) -> Result<String> {
        uuid::Uuid::parse_str(label).map_err(|_| anyhow!("Invalid account label."))?;
        Ok(format!("credentials-{label}.json"))
    }
    pub(crate) fn credentials(&self, account: &Account) -> Result<Credentials> {
        self.read(&Self::credential_name(&account.label)?)
    }
    pub(crate) fn save_credentials(
        &self,
        account: &Account,
        credentials: &Credentials,
    ) -> Result<()> {
        self.save(&Self::credential_name(&account.label)?, credentials)
    }
    pub(crate) fn active(&self) -> Result<Account> {
        let reg = self.registrations()?;
        reg.accounts
            .into_iter()
            .find(|a| Some(&a.label) == reg.active.as_ref())
            .ok_or_else(|| anyhow!("No active ChatGPT account; run chat_auth login."))
    }
    pub fn status(&self) -> Result<Account> {
        let account = self.active()?;
        self.credentials(&account)?.check_scopes()?;
        Ok(account)
    }
    pub fn accounts(&self) -> Result<Vec<Account>> {
        Ok(self.registrations()?.accounts)
    }
    pub(crate) fn clear_credentials(&self, account: &Account) -> Result<()> {
        let path = self.root.join(Self::credential_name(&account.label)?);
        self.check_private(&path, false)?;
        fs::remove_file(path).map_err(|_| anyhow!("Cannot remove local credentials."))?;
        Ok(())
    }
}
