//! Explicit interactive native login. No Task/Run, host login discovery, or
//! credential bytes in the request ledger. Resource ownership precedes I/O.
use crate::{
    Error, Result,
    cli_environment::CliEnvironment,
    connection::{ConnectionSpec, ConnectionVersion, ModelConnection},
    database::DatabaseClient,
    model::Command,
    store::{Store, load, new_id, revision, save, text},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliLogin {
    pub id: String,
    pub request_id: String,
    pub environment_id: String,
    pub worker_id: String,
    pub workspace_id: String,
    pub ownership_token: String,
    pub engine_id: String,
    pub image: String,
    pub generation: u64,
    pub state: String,
    pub code: String,
    pub creation_authorized: bool,
    pub pid: Option<u32>,
    pub process_identity: Option<String>,
    pub resources_stopped: bool,
}
impl CliLogin {
    fn names(&self) -> [String; 4] {
        [
            format!("atelier-login-{}", self.id),
            format!("atelier-login-proxy-{}", self.id),
            format!("atelier-login-inner-{}", self.id),
            format!("atelier-login-outer-{}", self.id),
        ]
    }
    fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("atelier.workspace".into(), self.workspace_id.clone()),
            ("atelier.login".into(), self.id.clone()),
            ("atelier.owner".into(), self.ownership_token.clone()),
        ])
    }
}

impl Store {
    fn begin_cli_login(
        &mut self,
        request: &str,
        command: &Command,
        interactive: bool,
    ) -> Result<(CliLogin, Option<CliEnvironment>)> {
        text(request, "requestId", 128)?;
        let Command::ConnectionLogin {
            id,
            revision: expected,
            version,
            worker,
        } = command
        else {
            return Err(Error::Invalid("不是 CLI 登录请求".into()));
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
        if let Some((hash, data)) = prior {
            if hash != fingerprint {
                return Err(Error::Conflict("同 requestId 的登录目标不同".into()));
            }
            let data: Value = serde_json::from_str(&data)?;
            return Ok((serde_json::from_value(data["login"].clone())?, None));
        }
        if !interactive {
            return Err(Error::Invalid(
                "首次登录需要交互终端，不能使用 --json 或从聊天传入凭据；已有请求可查询或核对"
                    .into(),
            ));
        }
        let connection: ModelConnection = load(&tx, "connections", id)?;
        revision(connection.revision, *expected)?;
        let selected: ConnectionVersion = load(
            &tx,
            "connection_versions",
            version.as_deref().unwrap_or(&connection.current_version),
        )?;
        if selected.connection_id != *id {
            return Err(Error::Forbidden("登录版本不属于该连接".into()));
        }
        selected.specification.validate()?;
        if !matches!(selected.specification, ConnectionSpec::AgentCli { .. }) {
            return Err(Error::Invalid("API 连接使用 credential 入口".into()));
        }
        let configuration = crate::content::digest(&serde_json::to_vec(&(worker, &selected.id))?);
        let mut environment: CliEnvironment = load(&tx, "cli_environments", &configuration)?;
        if environment.worker_id != *worker
            || environment.connection_version != selected.id
            || environment.state != "prepared"
        {
            return Err(Error::Conflict(
                "先完成该 Worker 和连接版本的专用环境准备".into(),
            ));
        }
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE json_extract(data,'$.worker_id')=?1 AND state IN ('prepared','running','unknown')) OR EXISTS(SELECT 1 FROM cli_logins WHERE json_extract(data,'$.workerId')=?1 AND json_extract(data,'$.resourcesStopped')=0)",[worker],|r|r.get(0))?;
        if busy {
            return Err(Error::Conflict(
                "该成员仍有未核对执行或登录；先核对旧资源".into(),
            ));
        }
        environment.login_generation = environment
            .login_generation
            .checked_add(1)
            .ok_or_else(|| Error::Conflict("登录代次已耗尽".into()))?;
        environment.login_material_ready = false;
        environment.state = "logging_in".into();
        environment.code = "login_pending".into();
        let record = CliLogin {
            id: new_id(),
            request_id: request.into(),
            environment_id: environment.id.clone(),
            worker_id: worker.clone(),
            workspace_id: tx.query_row("SELECT id FROM workspace", [], |r| r.get(0))?,
            ownership_token: new_id(),
            engine_id: environment
                .engine_id
                .clone()
                .ok_or_else(|| Error::Unavailable("环境缺少 Docker 引擎归属".into()))?,
            image: environment.image.clone(),
            generation: environment.login_generation,
            state: "prepared".into(),
            code: "login_pending".into(),
            creation_authorized: false,
            pid: None,
            process_identity: None,
            resources_stopped: false,
        };
        save(&tx, "cli_environments", &environment.id, &environment)?;
        save(&tx, "cli_logins", &record.id, &record)?;
        tx.execute(
            "INSERT INTO requests(actor,id,fingerprint,command,result) VALUES(?1,?2,?3,?4,?5)",
            params![
                actor,
                request,
                fingerprint,
                serde_json::to_string(command)?,
                json!({"login":record}).to_string()
            ],
        )?;
        tx.commit()?;
        Ok((record, Some(environment)))
    }
    fn save_cli_login(&mut self, record: &CliLogin) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let prior: CliLogin = load(&tx, "cli_logins", &record.id)?;
        if prior.resources_stopped || prior.ownership_token != record.ownership_token {
            return Err(Error::Conflict("登录资源依据已变化".into()));
        }
        save(&tx, "cli_logins", &record.id, record)?;
        if tx.execute(
            "UPDATE requests SET result=?2 WHERE actor=(SELECT self_id FROM workspace) AND id=?1",
            params![record.request_id, json!({"login":record}).to_string()],
        )? != 1
        {
            return Err(Error::Conflict("登录请求记录缺失".into()));
        }
        if record.resources_stopped {
            let mut environment: CliEnvironment =
                load(&tx, "cli_environments", &record.environment_id)?;
            if environment.login_generation != record.generation {
                return Err(Error::Conflict("登录代次已变化".into()));
            }
            environment.state = "prepared".into();
            environment.code = record.code.clone();
            environment.login_material_ready = record.code == "login_material_saved_unchecked";
            save(&tx, "cli_environments", &environment.id, &environment)?;
        }
        tx.commit()?;
        Ok(())
    }
}

async fn cleanup(record: &CliLogin) -> Result<()> {
    // Before this durable permit is granted, the private launcher cannot create
    // anything. A crash between spawn and PID registration therefore owns no
    // containers and cannot turn a missing PID into a false authenticated state.
    if !record.creation_authorized {
        return Ok(());
    }
    let pid = record
        .pid
        .ok_or_else(|| Error::Conflict("登录缺少进程归属，保留未知资源".into()))?;
    if !crate::api_driver::group_absent(pid).await? {
        return Err(Error::Conflict("登录创建进程组仍在，尚不能核对容器".into()));
    }
    let names = record.names();
    crate::cli_resources::cleanup_named(
        &record.engine_id,
        &record.labels(),
        names.each_ref().map(String::as_str),
    )
    .await
}
fn command_environment(command: &mut tokio::process::Command) {
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}
pub(crate) fn login_material(environment: &CliEnvironment, workspace: &Path) -> bool {
    let path = environment.directory(workspace).join("login/auth.json");
    let Ok(info) = fs::symlink_metadata(path) else {
        return false;
    };
    if !info.is_file()
        || info.file_type().is_symlink()
        || info.len() == 0
        || info.len() > 1024 * 1024
    {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if info.permissions().mode() & 0o077 != 0 {
            return false;
        }
    }
    true
}

async fn perform(
    store: &mut Store,
    record: &mut CliLogin,
    environment: &CliEnvironment,
) -> Result<bool> {
    environment.validate_storage(&store.workspace_path)?;
    let version = store.connection_version(&environment.connection_version)?;
    let ConnectionSpec::AgentCli {
        runtime,
        model,
        egress_hosts: Some(hosts),
        ..
    } = version.specification
    else {
        return Err(Error::Invalid("CLI 登录配置缺失".into()));
    };
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("adapters/milkie/dist/src/cli-login-main.js");
    if !entry.is_file() {
        return Err(Error::Unavailable("CLI 登录接入尚未构建".into()));
    }
    let mut bootstrap = json!({"loginId":record.id,"workspaceId":record.workspace_id,"ownershipToken":record.ownership_token,"engineId":record.engine_id,
        "image":record.image,"runtime":runtime,"configDirectory":environment.directory(&store.workspace_path).join("login"),"hosts":hosts});
    if let Some(model) = model {
        bootstrap["model"] = json!(model);
    }
    let mut input = serde_json::to_vec(&bootstrap)?;
    if input.len() > 8191 {
        return Err(Error::Invalid("登录配置超过私有通道上限".into()));
    }
    input.push(b'\n');
    let mut cmd = tokio::process::Command::new("node");
    command_environment(&mut cmd);
    cmd.arg(entry)
        .arg("--atelier-login")
        .arg(&record.id)
        .current_dir(&store.workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let mut helper = cmd.spawn()?;
    // Child::wait closes the child's stored stdin before waiting. Keep this
    // lifetime pipe separately so observing helper exit cannot revoke login.
    let mut control = helper.stdin.take().expect("private stdin");
    let operation=async {
        let pid=helper.id().ok_or_else(||Error::Unavailable("登录创建进程没有标识".into()))?;
        record.pid=Some(pid);record.process_identity=Some(crate::api_driver::process_identity(pid).await?);
        record.creation_authorized=true;record.state="running".into();store.save_cli_login(record)?;
        control.write_all(&input).await?;
        let stdout=helper.stdout.take().expect("private stdout");
        let mut reader=BufReader::new(stdout.take(1025));let mut line=Vec::new();
        tokio::time::timeout(Duration::from_secs(60),reader.read_until(b'\n',&mut line)).await.map_err(|_|Error::Unavailable("登录环境创建超时".into()))??;
        if line.len()>1024 || !line.ends_with(b"\n") {return Err(Error::Unavailable("登录创建回执无效".into()));}
        let ready:Value=serde_json::from_slice(&line)?;let names=record.names();
        if ready!=json!({"state":"created","containerName":names[0]}) {return Err(Error::Unavailable("登录容器归属回执不符".into()));}
        if runtime=="pi" {eprintln!("Pi 专用登录：输入 /login 并按提示认证，完成后退出 Pi。仅修改此 Worker 的专用登录存储。");}
        else {eprintln!("Grok 专用登录：按设备认证提示完成登录。仅修改此 Worker 的专用登录存储。");}
        // Foreground attach inherits the real user terminal. It cannot create
        // containers; only the separately registered helper group can do so.
        let mut attach_cmd=tokio::process::Command::new("docker");command_environment(&mut attach_cmd);
        let mut attach=attach_cmd.args(["start","-ai",&names[0]]).stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit()).kill_on_drop(true).spawn()?;
        let result=tokio::select! {
            status=attach.wait()=>status.map(|s|s.success()).map_err(Error::from),
            _=helper.wait()=>Err(Error::Unavailable("登录隔离进程已退出".into())),
            _=tokio::time::sleep(Duration::from_secs(30*60))=>Err(Error::Unavailable("登录超过 30 分钟期限".into())),
        };
        if attach.try_wait()?.is_none() {attach.kill().await?;attach.wait().await?;}
        result
    }.await;
    drop(control);
    match tokio::time::timeout(Duration::from_secs(10), helper.wait()).await {
        Ok(status) => {
            status?;
        }
        Err(_) => {
            helper.kill().await?;
            helper.wait().await?;
        }
    }
    operation
}

/// Existing request replay only reconciles its resources; it never logs in a
/// second time. A new attempt requires a new request ID and a user terminal.
pub async fn login(
    store: &mut Store,
    request: &str,
    command: &Command,
    interactive: bool,
) -> Result<Value> {
    let _lock = crate::cli_environment::lock(&store.workspace_path)?;
    let (mut record, environment) = store.begin_cli_login(request, command, interactive)?;
    if record.resources_stopped {
        return Ok(json!({"login":record}));
    }
    let (native_success, code) = if let Some(environment) = environment.as_ref() {
        match perform(store, &mut record, environment).await {
            Ok(true) => (true, "login_material_saved_unchecked"),
            Ok(false) => (false, "native_login_failed"),
            Err(_) => (false, "login_interrupted"),
        }
    } else {
        (false, "login_interrupted")
    };
    record.resources_stopped = cleanup(&record).await.is_ok();
    record.state = if record.resources_stopped {
        "stopped"
    } else {
        "unknown"
    }
    .into();
    record.code = if !record.resources_stopped {
        "resources_unresolved"
    } else if native_success
        && environment
            .as_ref()
            .is_some_and(|e| login_material(e, &store.workspace_path))
    {
        code
    } else if native_success {
        "login_material_missing"
    } else {
        code
    }
    .into();
    store.save_cli_login(&record)?;
    Ok(json!({"login":record}))
}

/// Service/offline reconciliation, with no terminal, provider request or new
/// login attempt. An interactive owner holding the environment lock is skipped.
pub(crate) async fn recover(client: &DatabaseClient, workspace: &Path) -> Result<usize> {
    let pending=client.call(|store|Ok(store.connection.query_row("SELECT EXISTS(SELECT 1 FROM cli_logins WHERE json_extract(data,'$.resourcesStopped')=0)",[],|r|r.get::<_,bool>(0))?)).await?;
    if !pending {
        return Ok(0);
    }
    let path = workspace.to_path_buf();
    let lock = match tokio::task::spawn_blocking(move || crate::cli_environment::lock(&path))
        .await
        .map_err(|_| Error::Unavailable("登录核对线程退出".into()))?
    {
        Ok(lock) => lock,
        Err(Error::Conflict(_)) => return Ok(0),
        Err(e) => return Err(e),
    };
    let records = client
        .call(|store| {
            let mut stmt = store.connection.prepare(
                "SELECT data FROM cli_logins WHERE json_extract(data,'$.resourcesStopped')=0",
            )?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows.into_iter()
                .map(|s| serde_json::from_str::<CliLogin>(&s).map_err(Into::into))
                .collect::<Result<Vec<_>>>()
        })
        .await?;
    let mut count = 0;
    for mut record in records {
        let stopped = cleanup(&record).await.is_ok();
        if !stopped && record.state == "unknown" && record.code == "resources_unresolved" {
            continue;
        }
        record.state = if stopped { "stopped" } else { "unknown" }.into();
        record.resources_stopped = stopped;
        record.code = if stopped {
            "login_interrupted"
        } else {
            "resources_unresolved"
        }
        .into();
        client
            .call(move |store| store.save_cli_login(&record))
            .await?;
        if stopped {
            count += 1;
        }
    }
    drop(lock);
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, Store, Command, String) {
        let directory = tempfile::tempdir().unwrap();
        Store::init(directory.path(), "登录事务测试").unwrap();
        let mut store = Store::open(directory.path()).unwrap();
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
                    name: "测试成员".into(),
                    description: String::new(),
                    connection: Some(id.clone()),
                },
            )
            .unwrap();
        let configuration = worker["execution_config"].as_str().unwrap().to_string();
        // A synthetic prepared environment tests DB contracts only. The live
        // Docker test separately verifies real storage, terminal and cleanup.
        let environment: CliEnvironment = serde_json::from_value(json!({
            "id":configuration,"workerId":worker["id"],
            "connectionVersion":connection["connection"]["current_version"],
            "runtime":"pi","image":format!("sha256:{}","a".repeat(64)),
            "engineId":"synthetic-engine","state":"prepared","code":"fixture",
            "loginGeneration":7,"loginMaterialReady":true,"everPrepared":true,
            "preparationRequestId":"fixture"
        }))
        .unwrap();
        save(
            &store.connection,
            "cli_environments",
            &configuration,
            &environment,
        )
        .unwrap();
        (
            directory,
            store,
            Command::ConnectionLogin {
                id,
                revision: 1,
                version: None,
                worker: worker["id"].as_str().unwrap().into(),
            },
            configuration,
        )
    }

    #[test]
    fn login_reservation_is_atomic_and_invalidates_previous_material_only_on_commit() {
        let (_directory, mut store, command, configuration) = fixture();
        assert!(store.begin_cli_login("login", &command, false).is_err());
        assert_eq!(
            store
                .cli_environment(&configuration)
                .unwrap()
                .unwrap()
                .login_generation,
            7
        );
        store.connection.execute_batch("CREATE TRIGGER reject_login BEFORE INSERT ON requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(store.begin_cli_login("login", &command, true).is_err());
        let unchanged = store.cli_environment(&configuration).unwrap().unwrap();
        assert!(unchanged.login_material_ready);
        assert_eq!(unchanged.state, "prepared");
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM cli_logins", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store
            .connection
            .execute_batch("DROP TRIGGER reject_login")
            .unwrap();
        let (record, environment) = store.begin_cli_login("login", &command, true).unwrap();
        assert_eq!(record.generation, 8);
        assert!(!environment.unwrap().login_material_ready);
        assert!(store.begin_cli_login("competing", &command, true).is_err());
        let (replay, environment) = store.begin_cli_login("login", &command, false).unwrap();
        assert_eq!(replay.id, record.id);
        assert!(environment.is_none());
        assert_eq!(store.list("task").unwrap(), json!([]));
        assert_eq!(
            store
                .connection
                .query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn interrupted_ungranted_login_reconciles_without_authentication_or_new_attempt() {
        let (directory, mut store, command, configuration) = fixture();
        let (record, _) = store.begin_cli_login("login", &command, true).unwrap();
        assert!(!record.creation_authorized);
        drop(store);
        let mut store = Store::open(directory.path()).unwrap();
        let result = login(&mut store, "login", &command, false).await.unwrap();
        assert_eq!(result["login"]["id"], record.id);
        assert_eq!(result["login"]["code"], "login_interrupted");
        assert_eq!(result["login"]["resourcesStopped"], true);
        assert_eq!(result["login"]["creationAuthorized"], false);
        assert_eq!(
            login(&mut store, "login", &command, false).await.unwrap(),
            result
        );
        let environment = store.cli_environment(&configuration).unwrap().unwrap();
        assert!(!environment.login_material_ready);
        assert_eq!(environment.login_generation, 8);
        let (next, _) = store
            .begin_cli_login("new-attempt", &command, true)
            .unwrap();
        assert_eq!(next.generation, 9);
        assert_ne!(next.id, record.id);
    }
}
