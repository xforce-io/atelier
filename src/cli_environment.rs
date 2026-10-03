//! Explicit Worker-owned CLI storage. Preparing a directory/image is distinct
//! from authenticating, testing a connection, or accepting business work.
use crate::{
    Error, Result,
    connection::{ConnectionSpec, ConnectionVersion, ExecutionConfiguration, ModelConnection},
    model::{Command, Worker, WorkerKind},
    store::{Store, load, revision, save, text},
};
use fs2::FileExt;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliEnvironment {
    pub id: String,
    pub worker_id: String,
    pub connection_version: String,
    pub runtime: String,
    pub image: String,
    pub engine_id: Option<String>,
    pub state: String,
    pub code: String,
    pub login_generation: u64,
    ever_prepared: bool,
    preparation_request_id: String,
}
impl CliEnvironment {
    pub fn directory(&self, workspace: &Path) -> PathBuf {
        workspace.join("cli-environments").join(&self.id)
    }
}

/// The same lock must be held by login and by an executing CLI using this
/// workspace's private login material. No second OS thread waits on the lock.
pub(crate) fn lock(workspace: &Path) -> Result<File> {
    let path = workspace.join("cli-environment.lock");
    match fs::symlink_metadata(&path) {
        Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
            return Err(Error::Invalid("CLI 环境锁无效".into()));
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
    file.try_lock_exclusive().map_err(|_| {
        Error::Conflict("CLI 环境正在被准备、登录或执行使用，请稍后按原 requestId 重试".into())
    })?;
    Ok(file)
}

impl Store {
    pub fn cli_environment(&self, configuration: &str) -> Result<Option<CliEnvironment>> {
        let data: Option<String> = self
            .connection
            .query_row(
                "SELECT data FROM cli_environments WHERE id=?1",
                [configuration],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    fn begin_preparation(
        &mut self,
        request: &str,
        command: &Command,
    ) -> Result<(CliEnvironment, bool)> {
        text(request, "requestId", 128)?;
        let Command::ConnectionPrepare {
            id,
            revision: expected,
            version,
            worker,
        } = command
        else {
            return Err(Error::Invalid("不是 CLI 环境准备请求".into()));
        };
        let fingerprint = crate::content::digest(&serde_json::to_vec(command)?);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor: String = tx.query_row("SELECT self_id FROM workspace", [], |r| r.get(0))?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_requests WHERE request_id=?1)",
            [request],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Conflict("requestId 已用于凭据操作".into()));
        }
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT fingerprint,result FROM requests WHERE actor=?1 AND id=?2",
                params![actor, request],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((hash, result)) = prior {
            if hash != fingerprint {
                return Err(Error::Conflict("同 requestId 的准备目标不同".into()));
            }
            let saved: Value = serde_json::from_str(&result)?;
            let record: CliEnvironment = serde_json::from_value(saved["environment"].clone())?;
            if record.state != "preparing" {
                return Ok((record, false));
            }
            let current: CliEnvironment = load(&tx, "cli_environments", &record.id)?;
            if current.preparation_request_id != request || current.state != "preparing" {
                return Err(Error::Conflict("准备操作依据已变化".into()));
            }
            return Ok((current, true));
        }
        let connection: ModelConnection = load(&tx, "connections", id)?;
        revision(connection.revision, *expected)?;
        let selected: ConnectionVersion = load(
            &tx,
            "connection_versions",
            version.as_deref().unwrap_or(&connection.current_version),
        )?;
        if selected.connection_id != *id {
            return Err(Error::Forbidden("准备版本不属于该连接".into()));
        }
        selected.specification.validate()?;
        let ConnectionSpec::AgentCli {
            runtime,
            image: Some(image),
            egress_hosts: Some(_),
            ..
        } = &selected.specification
        else {
            return Err(Error::Invalid("CLI 准备需要固定镜像和显式出站策略".into()));
        };
        if !matches!(runtime.as_str(), "pi" | "grok-cli") {
            return Err(Error::Unavailable("当前仅支持 pi 与 grok-cli".into()));
        }
        let member: Worker = load(&tx, "workers", worker)?;
        if member.kind != WorkerKind::Agent {
            return Err(Error::Invalid("CLI 准备须指定数字员工".into()));
        }
        // Explicit old-version repair is allowed only for an existing immutable
        // configuration which already belongs to this Worker and this version.
        let configuration = crate::content::digest(&serde_json::to_vec(&(worker, &selected.id))?);
        let bound: ExecutionConfiguration = load(&tx, "execution_configs", &configuration)?;
        if bound.worker_id != *worker || bound.connection_version != selected.id {
            return Err(Error::Forbidden("成员未绑定该执行配置".into()));
        }
        let active:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE json_extract(data,'$.worker_id')=?1 AND state IN ('prepared','running','unknown'))",[worker],|r|r.get(0))?;
        if active {
            return Err(Error::Conflict(
                "成员还有活动或未知 Run，须先停止并核对".into(),
            ));
        }
        let previous: Option<String> = tx
            .query_row(
                "SELECT data FROM cli_environments WHERE id=?1",
                [&configuration],
                |r| r.get(0),
            )
            .optional()?;
        let mut record = if let Some(data) = previous {
            let record: CliEnvironment = serde_json::from_str(&data)?;
            if record.state == "preparing" {
                return Err(Error::Conflict("先按原 requestId 完成已有准备操作".into()));
            }
            record
        } else {
            CliEnvironment {
                id: configuration,
                worker_id: worker.clone(),
                connection_version: selected.id,
                runtime: runtime.clone(),
                image: image.clone(),
                engine_id: None,
                state: "preparing".into(),
                code: "preparation_pending".into(),
                login_generation: 0,
                ever_prepared: false,
                preparation_request_id: request.into(),
            }
        };
        record.state = "preparing".into();
        record.code = "preparation_pending".into();
        record.preparation_request_id = request.into();
        save(&tx, "cli_environments", &record.id, &record)?;
        tx.execute(
            "INSERT INTO requests(actor,id,fingerprint,command,result) VALUES(?1,?2,?3,?4,?5)",
            params![
                actor,
                request,
                fingerprint,
                serde_json::to_string(command)?,
                json!({"environment":record}).to_string()
            ],
        )?;
        tx.commit()?;
        Ok((record, true))
    }
    fn finish_preparation(
        &mut self,
        record: &CliEnvironment,
        outcome: std::result::Result<String, &str>,
    ) -> Result<CliEnvironment> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut current: CliEnvironment = load(&tx, "cli_environments", &record.id)?;
        if current.preparation_request_id != record.preparation_request_id
            || current.state != "preparing"
        {
            return Err(Error::Conflict("CLI 准备依据已变化".into()));
        }
        match outcome {
            Ok(engine) => {
                current.state = "prepared".into();
                current.code = "prepared_not_authenticated".into();
                current.engine_id = Some(engine);
                current.ever_prepared = true;
            }
            Err(code) => {
                current.state = "failed".into();
                current.code = code.into();
            }
        }
        save(&tx, "cli_environments", &current.id, &current)?;
        let changed = tx.execute(
            "UPDATE requests SET result=?2 WHERE actor=(SELECT self_id FROM workspace) AND id=?1",
            params![
                current.preparation_request_id,
                json!({"environment":current}).to_string()
            ],
        )?;
        if changed != 1 {
            return Err(Error::Conflict("准备请求记录缺失".into()));
        }
        tx.commit()?;
        Ok(current)
    }
}

fn private_directory(path: &Path, create: bool) -> Result<()> {
    if create {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(path) {
            Ok(()) => {
                File::open(path)?.sync_all()?;
                File::open(path.parent().expect("private directory parent"))?.sync_all()?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(Error::Invalid("CLI 专用目录无效".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if m.permissions().mode() & 0o077 != 0 {
            return Err(Error::Invalid("CLI 专用目录须为私有目录".into()));
        }
    }
    Ok(())
}
fn prepare_storage(workspace: &Path, record: &CliEnvironment, engine: &str) -> Result<()> {
    let root = workspace.join("cli-environments");
    let own = record.directory(workspace);
    for path in [&root, &own, &own.join("login")] {
        private_directory(path, !record.ever_prepared)?;
    }
    let binding = json!({"version":1,"environmentId":record.id,"workerId":record.worker_id,"connectionVersion":record.connection_version,"runtime":record.runtime,"image":record.image,"engineId":engine});
    let path = own.join("binding.json");
    match fs::symlink_metadata(&path) {
        Ok(m) => {
            if !m.is_file()
                || m.file_type().is_symlink()
                || m.len() > 8192
                || serde_json::from_slice::<Value>(&fs::read(&path)?)? != binding
            {
                return Err(Error::Conflict("CLI 专用环境绑定不符或已损坏".into()));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !record.ever_prepared => {
            if fs::read_dir(own.join("login"))?.next().is_some() {
                return Err(Error::Conflict(
                    "未绑定的 CLI 环境存在登录内容，拒绝接管".into(),
                ));
            }
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            file.write_all(&serde_json::to_vec(&binding)?)?;
            file.sync_all()?;
            File::open(own)?.sync_all()?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// No download, provider call, native CLI launch, or ambient login discovery.
/// The image must already be explicitly prepared and selected by digest.
pub async fn prepare(store: &mut Store, request: &str, command: &Command) -> Result<Value> {
    let _lock = lock(&store.workspace_path)?;
    let (record, pending) = store.begin_preparation(request, command)?;
    if !pending {
        return Ok(json!({"environment":record}));
    }
    let outcome = async {
        let engine = crate::cli_resources::engine_identity()
            .await
            .map_err(|_| "docker_unavailable")?;
        if record.engine_id.as_ref().is_some_and(|id| id != &engine) {
            return Err("docker_engine_changed");
        }
        let bytes = crate::cli_resources::docker(&["image", "inspect", &record.image])
            .await
            .map_err(|_| "image_unavailable")?;
        let image: Value = serde_json::from_slice(&bytes).map_err(|_| "image_invalid")?;
        if image.as_array().is_none_or(|images| images.len() != 1)
            || image[0]["Os"] != "linux"
            || image[0]["Config"]["Labels"]["atelier.milkie"] != crate::channel::MILKIE_COMMIT
            || image[0]["Config"]["Labels"]["atelier.adapter.protocol"] != "2"
        {
            return Err("image_incompatible");
        }
        let workspace = store.workspace_path.clone();
        let environment = record.clone();
        let expected = engine.clone();
        tokio::task::spawn_blocking(move || prepare_storage(&workspace, &environment, &expected))
            .await
            .map_err(|_| "storage_unavailable")?
            .map_err(|_| "private_environment_invalid")?;
        Ok(engine)
    }
    .await;
    let result = store.finish_preparation(&record, outcome)?;
    Ok(json!({"environment":result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, Command) {
        let dir = tempfile::tempdir().unwrap();
        Store::init(dir.path(), "环境测试").unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let connection = store
            .execute(
                "connection",
                &Command::ConnectionCreate {
                    name: "CLI".into(),
                    specification: ConnectionSpec::AgentCli {
                        runtime: "pi".into(),
                        model: None,
                        image: Some(format!("sha256:{}", "a".repeat(64))),
                        egress_hosts: Some(vec!["example.com".into()]),
                    },
                },
            )
            .unwrap();
        let id = connection["connection"]["id"].as_str().unwrap().to_string();
        let worker = store
            .execute(
                "worker",
                &Command::WorkerCreate {
                    name: "成员".into(),
                    description: String::new(),
                    connection: Some(id.clone()),
                },
            )
            .unwrap();
        let command = Command::ConnectionPrepare {
            id,
            revision: 1,
            version: None,
            worker: worker["id"].as_str().unwrap().into(),
        };
        (dir, store, command)
    }
    #[test]
    fn preparation_is_durable_replayable_and_never_authenticates_or_creates_work() {
        let (dir, mut store, command) = fixture();
        let (record, pending) = store.begin_preparation("prepare", &command).unwrap();
        assert!(pending);
        assert_eq!(record.login_generation, 0);
        assert!(!record.ever_prepared);
        drop(store);
        let mut store = Store::open(dir.path()).unwrap();
        let (retry, pending) = store.begin_preparation("prepare", &command).unwrap();
        assert!(pending);
        assert_eq!(retry.id, record.id);
        assert!(store.begin_preparation("competing", &command).is_err());
        prepare_storage(dir.path(), &retry, "engine").unwrap();
        store.connection.execute_batch("CREATE TRIGGER reject_environment_finish BEFORE UPDATE ON requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(
            store
                .finish_preparation(&retry, Ok("engine".into()))
                .is_err()
        );
        assert_eq!(
            store.cli_environment(&record.id).unwrap().unwrap().state,
            "preparing"
        );
        store
            .connection
            .execute_batch("DROP TRIGGER reject_environment_finish")
            .unwrap();
        let saved = store
            .finish_preparation(&retry, Ok("engine".into()))
            .unwrap();
        let (cached, pending) = store.begin_preparation("prepare", &command).unwrap();
        assert!(!pending);
        assert_eq!(cached.id, saved.id);
        assert_eq!(cached.state, "prepared");
        assert_eq!(cached.code, "prepared_not_authenticated");
        assert_eq!(cached.login_generation, 0);
        assert_eq!(
            fs::read_dir(record.directory(dir.path()).join("login"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(store.list("task").unwrap(), json!([]));
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        let Command::ConnectionPrepare { id, .. } = &command else {
            unreachable!()
        };
        assert_eq!(
            store.connection_view(id).unwrap()["cliEnvironments"][0]["id"],
            record.id
        );
        assert!(
            !store.connection_view(id).unwrap()["executionSupported"]
                .as_bool()
                .unwrap()
        );
        assert!(
            store
                .execute(
                    "prepare",
                    &Command::WorkerCreate {
                        name: "冲突".into(),
                        description: String::new(),
                        connection: None
                    }
                )
                .is_err()
        );
    }
    #[test]
    fn preparation_never_adopts_foreign_login_or_recreates_lost_prepared_storage() {
        let (dir, mut store, command) = fixture();
        let (record, _) = store.begin_preparation("prepare", &command).unwrap();
        prepare_storage(dir.path(), &record, "engine").unwrap();
        let binding = record.directory(dir.path()).join("binding.json");
        let original = fs::read(&binding).unwrap();
        assert!(prepare_storage(dir.path(), &record, "other-engine").is_err());
        assert_eq!(fs::read(&binding).unwrap(), original);
        let saved = store
            .finish_preparation(&record, Ok("engine".into()))
            .unwrap();
        fs::remove_dir(saved.directory(dir.path()).join("login")).unwrap();
        assert!(prepare_storage(dir.path(), &saved, "engine").is_err());
        assert!(!saved.directory(dir.path()).join("login").exists());
        let second = tempfile::tempdir().unwrap();
        private_directory(&second.path().join("cli-environments"), true).unwrap();
        private_directory(&record.directory(second.path()), true).unwrap();
        private_directory(&record.directory(second.path()).join("login"), true).unwrap();
        fs::write(
            record.directory(second.path()).join("login/auth.json"),
            "synthetic-unowned",
        )
        .unwrap();
        assert!(prepare_storage(second.path(), &record, "engine").is_err());
        assert!(
            !record
                .directory(second.path())
                .join("binding.json")
                .exists()
        );
        #[cfg(unix)]
        {
            fs::remove_dir_all(record.directory(second.path()).join("login")).unwrap();
            std::os::unix::fs::symlink(dir.path(), record.directory(second.path()).join("login"))
                .unwrap();
            assert!(prepare_storage(second.path(), &record, "engine").is_err());
        }
    }
    #[test]
    fn prepare_rejects_wrong_worker_version_and_atomic_reservation_failure() {
        let (_dir, mut store, command) = fixture();
        let mut foreign = command.clone();
        if let Command::ConnectionPrepare { worker, .. } = &mut foreign {
            *worker = store.workspace().unwrap()["self"]["id"]
                .as_str()
                .unwrap()
                .into();
        }
        assert!(store.begin_preparation("human", &foreign).is_err());
        let mut stale = command.clone();
        if let Command::ConnectionPrepare { revision, .. } = &mut stale {
            *revision = 99;
        }
        assert!(store.begin_preparation("stale", &stale).is_err());
        store.connection.execute_batch("CREATE TRIGGER reject_environment_request BEFORE INSERT ON requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(store.begin_preparation("prepare", &command).is_err());
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM cli_environments", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store
            .connection
            .execute_batch("DROP TRIGGER reject_environment_request")
            .unwrap();
        let (record, _) = store.begin_preparation("prepare", &command).unwrap();
        assert!(store.begin_preparation("prepare", &foreign).is_err());
        let failed = store
            .finish_preparation(&record, Err("docker_unavailable"))
            .unwrap();
        assert_eq!(failed.state, "failed");
        assert!(!store.begin_preparation("prepare", &command).unwrap().1);
        assert!(
            store
                .begin_preparation("retry-after-repair", &command)
                .unwrap()
                .1
        );
    }
}
