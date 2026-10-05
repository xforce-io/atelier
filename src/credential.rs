//! Credentials stay in the macOS Keychain. SQLite contains only immutable
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
use std::{fs::OpenOptions, path::Path};

const SERVICE: &str = "io.xforce.atelier.api";
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
fn unavailable() -> Error {
    Error::Unavailable("Keychain 不可用或访问被拒绝；未回退到明文凭据".into())
}
#[cfg(target_os = "macos")]
fn read(account: &str) -> Result<Option<Vec<u8>>> {
    let _guard = security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
        .map_err(|_| unavailable())?;
    match security_framework::passwords::get_generic_password(SERVICE, account) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(_) => Err(unavailable()),
    }
}
#[cfg(not(target_os = "macos"))]
fn read(_account: &str) -> Result<Option<Vec<u8>>> {
    Err(unavailable())
}
#[cfg(target_os = "macos")]
fn write(account: &str, secret: &[u8]) -> Result<()> {
    let _guard = security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
        .map_err(|_| unavailable())?;
    security_framework::passwords::set_generic_password(SERVICE, account, secret)
        .map_err(|_| unavailable())
}
#[cfg(not(target_os = "macos"))]
fn write(_account: &str, _secret: &[u8]) -> Result<()> {
    Err(unavailable())
}
/// Trusted service only. Never expose the returned value to a member tool or log.
pub fn load_secret(reference: &CredentialReference) -> Result<String> {
    let bytes = read(&reference.account)?
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
    /// fingerprints. A per-workspace lock serializes Keychain publication.
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
        match read(&setting.reference.account)? {
            Some(existing) if existing != secret.as_bytes() => {
                return Err(Error::Conflict(
                    "同 requestId 的凭据内容不同；更换凭据须使用新请求".into(),
                ));
            }
            Some(_) => {}
            None if setting.published => {
                return Err(Error::Unavailable(
                    "该操作的 Keychain 内容已缺失；请使用新请求设置凭据".into(),
                ));
            }
            None => write(&setting.reference.account, secret.as_bytes())?,
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
            version: setting.version_selector,
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

#[cfg(target_os = "macos")]
fn remove(account: &str) -> Result<()> {
    let _guard = security_framework::os::macos::keychain::SecKeychain::disable_user_interaction()
        .map_err(|_| unavailable())?;
    match security_framework::passwords::delete_generic_password(SERVICE, account) {
        Ok(()) => Ok(()),
        Err(e) if e.code() == -25300 => Ok(()),
        Err(_) => Err(unavailable()),
    }
}
#[cfg(not(target_os = "macos"))]
fn remove(_account: &str) -> Result<()> {
    Err(unavailable())
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
        remove(&setting.reference.account)?;
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
