//! API credentials stay in workspace files. SQLite contains only immutable
//! operation identity, version binding and credential generation references.
use crate::{
    Error, Result,
    connection::{ConnectionSpec, ModelConnection},
    model::Command,
    store::{Store, load, text},
};
use fs2::FileExt;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CredentialReference {
    pub connection_version: String,
    pub generation: u64,
    pub account: String,
}
#[derive(Serialize, Deserialize)]
struct Setting {
    clear: bool,
    version_selector: Option<String>,
    connection_id: String,
    expected_revision: u64,
    previous_generation: u64,
    reference: CredentialReference,
    published: bool,
}

pub const CURRENT_BINARY_UNREADABLE: &str =
    "当前二进制无法读取已有 Keychain 项，请用当前二进制重新设置凭据";

fn unavailable() -> Error {
    Error::Unavailable("凭据文件无法读写".into())
}

fn credential_path(workspace: &Path, account: &str) -> Result<PathBuf> {
    if account.is_empty()
        || account.contains('/')
        || account.contains('\\')
        || account.contains('\0')
        || account == "."
        || account == ".."
    {
        return Err(Error::Invalid("凭据账号无效".into()));
    }
    Ok(workspace.join("credentials").join(account))
}

fn private_directory(path: &Path) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Invalid("凭据目录无效".into()));
        }
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path).map_err(|_| unavailable())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir(path).map_err(|_| unavailable())?;
    }
    Ok(())
}

fn read(workspace: &Path, account: &str) -> Result<Option<Vec<u8>>> {
    let path = credential_path(workspace, account)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(unavailable()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Error::Invalid("凭据文件无效".into()));
    }
    if metadata.len() > 16384 {
        return Err(unavailable());
    }
    std::fs::read(&path).map(Some).map_err(|_| unavailable())
}

fn write(workspace: &Path, account: &str, secret: &[u8]) -> Result<()> {
    let path = credential_path(workspace, account)?;
    let directory = path.parent().ok_or_else(unavailable)?;
    private_directory(directory)?;
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Error::Invalid("凭据文件无效".into()));
        }
    }
    let temporary = directory.join(format!(".{account}.tmp"));
    let mut file = std::fs::File::create(&temporary).map_err(|_| unavailable())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_| unavailable())?;
    }
    file.write_all(secret).map_err(|_| unavailable())?;
    file.sync_all().map_err(|_| unavailable())?;
    drop(file);
    std::fs::rename(&temporary, &path).map_err(|_| unavailable())?;
    Ok(())
}

/// Trusted service only. Never expose the returned value to a member tool or log.
pub fn load_secret(workspace: &Path, reference: &CredentialReference) -> Result<String> {
    let bytes = read(workspace, &reference.account)?
        .ok_or_else(|| Error::Unavailable("API 凭据缺失；请通过受保护入口重新设置".into()))?;
    let secret = String::from_utf8(bytes).map_err(|_| unavailable())?;
    validate(&secret)?;
    Ok(secret)
}
fn validate(secret: &str) -> Result<()> {
    if secret.is_empty()
        || secret.len() > 16384
        || secret.trim() != secret
        || secret.chars().any(char::is_control)
    {
        return Err(Error::Invalid(
            "API 凭据须为非空单行文本，最多 16 KiB".into(),
        ));
    }
    Ok(())
}
fn lock(workspace: &Path) -> Result<std::fs::File> {
    let path = workspace.join("credential.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
            return Err(Error::Invalid("凭据操作锁无效".into()));
        }
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    file.try_lock_exclusive().map_err(|e| {
        if e.kind() == std::io::ErrorKind::WouldBlock {
            Error::Conflict("另一个凭据操作进行中，请按原 requestId 重试".into())
        } else {
            e.into()
        }
    })?;
    Ok(file)
}
impl Store {
    pub fn credential_reference(&self, version: &str) -> Result<Option<CredentialReference>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM credentials WHERE version_id=?1",
                [version],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    /// Management entry only; the secret is never part of Command or request
    /// fingerprints. A per-workspace lock serializes credential file publication.
    pub fn set_credential(
        &mut self,
        request: &str,
        id: &str,
        revision: u64,
        secret: &str,
    ) -> Result<Value> {
        self.set_credential_version(request, id, revision, None, secret)
    }
    pub fn set_credential_version(
        &mut self,
        request: &str,
        id: &str,
        revision: u64,
        version: Option<&str>,
        secret: &str,
    ) -> Result<Value> {
        text(request, "requestId", 128)?;
        validate(secret)?;
        let _lock = lock(&self.workspace_path)?;
        let actor: String =
            self.connection
                .query_row("SELECT self_id FROM workspace", [], |r| r.get(0))?;
        let prior: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM credential_requests WHERE request_id=?1",
                [request],
                |r| r.get(0),
            )
            .optional()?;
        let mut setting = if let Some(data) = prior {
            let s: Setting = serde_json::from_str(&data)?;
            if s.clear
                || s.connection_id != id
                || s.expected_revision != revision
                || s.version_selector.as_deref() != version
            {
                return Err(Error::Conflict("同 requestId 的凭据操作目标不同".into()));
            }
            s
        } else {
            let tx = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let used: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests WHERE actor=?1 AND id=?2)",
                params![actor, request],
                |r| r.get(0),
            )?;
            if used {
                return Err(Error::Conflict("requestId 已用于其他操作".into()));
            }
            let connection: ModelConnection = load(&tx, "connections", id)?;
            crate::store::revision(connection.revision, revision)?;
            let selected: crate::connection::ConnectionVersion = load(
                &tx,
                "connection_versions",
                version.unwrap_or(&connection.current_version),
            )?;
            if selected.connection_id != id {
                return Err(Error::Forbidden("凭据目标版本不属于该连接".into()));
            }
            if !matches!(selected.specification, ConnectionSpec::Api { .. }) {
                return Err(Error::Invalid(
                    "agent CLI 使用独立登录，不设置 API 凭据".into(),
                ));
            }
            let old: Option<String> = tx
                .query_row(
                    "SELECT data FROM credentials WHERE version_id=?1",
                    [&selected.id],
                    |r| r.get(0),
                )
                .optional()?;
            let previous = old
                .map(|s| serde_json::from_str::<CredentialReference>(&s))
                .transpose()?
                .map_or(0, |c| c.generation);
            let workspace: String = tx.query_row("SELECT id FROM workspace", [], |r| r.get(0))?;
            let last:u64=tx.query_row("SELECT COALESCE(MAX(json_extract(data,'$.reference.generation')),0) FROM credential_requests WHERE json_extract(data,'$.reference.connection_version')=?1",[&selected.id],|r|r.get(0))?;
            let s = Setting {
                clear: false,
                version_selector: version.map(str::to_string),
                connection_id: id.into(),
                expected_revision: revision,
                previous_generation: previous,
                reference: CredentialReference {
                    connection_version: selected.id,
                    generation: last + 1,
                    account: format!("{workspace}:{}", uuid::Uuid::new_v4()),
                },
                published: false,
            };
            tx.execute(
                "INSERT INTO credential_requests(request_id,data) VALUES(?1,?2)",
                params![request, serde_json::to_string(&s)?],
            )?;
            tx.commit()?;
            s
        };
        match read(&self.workspace_path, &setting.reference.account)? {
            Some(existing) if existing != secret.as_bytes() => {
                return Err(Error::Conflict(
                    "同 requestId 的凭据内容不同；更换凭据须使用新请求".into(),
                ));
            }
            Some(_) => {}
            None if setting.published => {
                return Err(Error::Unavailable(
                    "该操作的凭据文件已缺失；请使用新请求设置凭据".into(),
                ));
            }
            None => write(
                &self.workspace_path,
                &setting.reference.account,
                secret.as_bytes(),
            )?,
        }
        if setting.published {
            return self.request(request);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: ModelConnection = load(&tx, "connections", id)?;
        crate::store::revision(current.revision, revision)?;
        if setting.version_selector.is_none()
            && current.current_version != setting.reference.connection_version
        {
            return Err(Error::Conflict("凭据设置期间连接版本变化".into()));
        }
        let old: Option<String> = tx
            .query_row(
                "SELECT data FROM credentials WHERE version_id=?1",
                [&setting.reference.connection_version],
                |r| r.get(0),
            )
            .optional()?;
        let generation = old
            .map(|s| serde_json::from_str::<CredentialReference>(&s))
            .transpose()?
            .map_or(0, |c| c.generation);
        if generation != setting.previous_generation {
            return Err(Error::Conflict(
                "凭据代次已变化，旧设置请求不能覆盖新凭据".into(),
            ));
        }
        tx.execute("INSERT INTO credentials(version_id,data) VALUES(?1,?2) ON CONFLICT(version_id) DO UPDATE SET data=excluded.data",params![setting.reference.connection_version,serde_json::to_string(&setting.reference)?])?;
        setting.published = true;
        tx.execute(
            "UPDATE credential_requests SET data=?2 WHERE request_id=?1",
            params![request, serde_json::to_string(&setting)?],
        )?;
        let result = json!({"connectionId":id,"connectionVersion":setting.reference.connection_version,"credentialGeneration":setting.reference.generation,"credentialStored":true,"readiness":"unchecked"});
        let command = Command::CredentialSet {
            id: id.into(),
            revision,
            version: setting.version_selector.clone(),
        };
        let encoded = serde_json::to_string(&command)?;
        let fingerprint = crate::content::digest(encoded.as_bytes());
        tx.execute(
            "INSERT INTO requests(actor,id,fingerprint,command,result) VALUES(?1,?2,?3,?4,?5)",
            params![
                actor,
                request,
                fingerprint,
                encoded,
                serde_json::to_string(&result)?
            ],
        )?;
        tx.commit()?;
        Ok(result)
    }
}

fn remove(workspace: &Path, account: &str) -> Result<()> {
    let path = credential_path(workspace, account)?;
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(Error::Invalid("凭据文件无效".into()))
        }
        Ok(_) => std::fs::remove_file(&path).map_err(|_| unavailable()),
        Err(_) => Err(unavailable()),
    }
}

impl Store {
    /// Removes exactly the selected local credential, not the provider's token
    /// or other versions. Durable intent makes a lost deletion reply retryable.
    pub fn clear_credential(
        &mut self,
        request: &str,
        id: &str,
        revision: u64,
        version: Option<&str>,
    ) -> Result<Value> {
        text(request, "requestId", 128)?;
        let _lock = lock(&self.workspace_path)?;
        let actor: String =
            self.connection
                .query_row("SELECT self_id FROM workspace", [], |r| r.get(0))?;
        let prior: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM credential_requests WHERE request_id=?1",
                [request],
                |r| r.get(0),
            )
            .optional()?;
        let mut setting = if let Some(data) = prior {
            let s: Setting = serde_json::from_str(&data)?;
            if !s.clear
                || s.connection_id != id
                || s.expected_revision != revision
                || s.version_selector.as_deref() != version
            {
                return Err(Error::Conflict("同 requestId 的凭据操作不同".into()));
            }
            if s.published {
                return self.request(request);
            }
            s
        } else {
            let tx = self
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            if tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM requests WHERE actor=?1 AND id=?2)",
                params![actor, request],
                |r| r.get::<_, bool>(0),
            )? {
                return Err(Error::Conflict("requestId 已用于其他操作".into()));
            }
            let connection: ModelConnection = load(&tx, "connections", id)?;
            crate::store::revision(connection.revision, revision)?;
            let selected: crate::connection::ConnectionVersion = load(
                &tx,
                "connection_versions",
                version.unwrap_or(&connection.current_version),
            )?;
            if selected.connection_id != id {
                return Err(Error::Forbidden("凭据目标版本不属于该连接".into()));
            }
            let data: Option<String> = tx
                .query_row(
                    "SELECT data FROM credentials WHERE version_id=?1",
                    [&selected.id],
                    |r| r.get(0),
                )
                .optional()?;
            let reference: CredentialReference = serde_json::from_str(
                &data.ok_or_else(|| Error::NotFound("该连接版本尚无凭据".into()))?,
            )?;
            let s = Setting {
                clear: true,
                connection_id: id.into(),
                expected_revision: revision,
                previous_generation: reference.generation,
                reference,
                version_selector: version.map(str::to_string),
                published: false,
            };
            tx.execute(
                "INSERT INTO credential_requests(request_id,data) VALUES(?1,?2)",
                params![request, serde_json::to_string(&s)?],
            )?;
            tx.commit()?;
            s
        };
        remove(&self.workspace_path, &setting.reference.account)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM credentials WHERE version_id=?1 AND json_extract(data,'$.account')=?2",
            params![
                setting.reference.connection_version,
                setting.reference.account
            ],
        )?;
        setting.published = true;
        tx.execute(
            "UPDATE credential_requests SET data=?2 WHERE request_id=?1",
            params![request, serde_json::to_string(&setting)?],
        )?;
        let result = json!({"connectionId":id,"connectionVersion":setting.reference.connection_version,"credentialGeneration":setting.reference.generation,"credentialCleared":true,"readiness":"unchecked"});
        let command = Command::CredentialClear {
            id: id.into(),
            revision,
            version: setting.version_selector,
        };
        let encoded = serde_json::to_string(&command)?;
        tx.execute(
            "INSERT INTO requests(actor,id,fingerprint,command,result) VALUES(?1,?2,?3,?4,?5)",
            params![
                actor,
                request,
                crate::content::digest(encoded.as_bytes()),
                encoded,
                serde_json::to_string(&result)?
            ],
        )?;
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::{credential_path, read, write};
    use crate::Error;

    #[test]
    fn file_roundtrip_is_readable_without_a_keychain() {
        let directory = tempfile::tempdir().unwrap();
        let secret = b"synthetic-secret";
        write(directory.path(), "account-one", secret).unwrap();
        assert_eq!(
            read(directory.path(), "account-one").unwrap().unwrap(),
            secret
        );
        let stored =
            std::fs::read(directory.path().join("credentials").join("account-one")).unwrap();
        assert_eq!(stored, secret);
    }

    #[test]
    fn credential_account_cannot_escape_the_directory() {
        let directory = tempfile::tempdir().unwrap();
        assert!(matches!(
            credential_path(directory.path(), "../outside"),
            Err(Error::Invalid(_))
        ));
        assert!(matches!(
            write(directory.path(), "nested/account", b"secret"),
            Err(Error::Invalid(_))
        ));
    }
}
