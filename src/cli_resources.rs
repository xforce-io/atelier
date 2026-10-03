//! Durable ownership for CLI containers, their proxy and two private networks.
//! Registration is a trusted service operation, not a member tool. It does not
//! assert SDK readiness or launch a CLI. Recovery never trusts a process report.
use crate::{
    Error, Result,
    connection::{ConnectionSpec, ConnectionVersion, ExecutionConfiguration},
    database::DatabaseClient,
    model::{Run, Task},
    store::{Store, load, new_id},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::io::AsyncReadExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CliResources {
    pub run_id: String,
    pub workspace_id: String,
    pub ownership_token: String,
    pub engine_id: String,
    pub execution_container: String,
    pub proxy_container: String,
    pub internal_network: String,
    pub egress_network: String,
    pub image: String,
    #[serde(default)]
    pub egress_hosts: Vec<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    #[serde(default)]
    pub resume: Option<bool>,
    pub resources_stopped: bool,
    pub diagnostic: Option<String>,
}
impl CliResources {
    /// Apply all labels at creation, before starting a process or connecting a
    /// container. The names are also committed before the first Docker request.
    pub fn labels(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("atelier.workspace".into(), self.workspace_id.clone()),
            ("atelier.run".into(), self.run_id.clone()),
            ("atelier.owner".into(), self.ownership_token.clone()),
        ])
    }
}
fn read(db: &Connection, id: &str) -> Result<Option<CliResources>> {
    let data: Option<String> = db
        .query_row(
            "SELECT data FROM cli_resources WHERE run_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    data.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
fn save(db: &Connection, record: &CliResources) -> Result<()> {
    db.execute("INSERT INTO cli_resources(run_id,data) VALUES(?1,?2) ON CONFLICT(run_id) DO UPDATE SET data=excluded.data",
        params![record.run_id, serde_json::to_string(record)?])?;
    Ok(())
}
pub(crate) fn before_stop(db: &Connection, run: &Run) -> Result<()> {
    if read(db, &run.id)?.is_some_and(|r| !r.resources_stopped) {
        return Err(Error::Conflict(
            "CLI 容器、代理或网络尚未核对，不能释放 Run".into(),
        ));
    }
    Ok(())
}
impl Store {
    pub fn cli_resources(&self, id: &str) -> Result<Option<CliResources>> {
        read(&self.connection, id)
    }
    /// Persist before any CLI adapter/container creation. The adapter must run
    /// in the process group subsequently registered by runtime_child_started.
    /// Capability preparation is the caller's responsibility, as with claim.
    pub fn runtime_begin_cli_resources(
        &mut self,
        epoch: &str,
        id: &str,
        engine_id: &str,
    ) -> Result<CliResources> {
        self.begin_cli_resources(epoch, id, engine_id, None)
    }

    /// Production launch binds the native context in the same transaction as
    /// resource ownership and Run intent, before any external resource exists.
    pub fn runtime_begin_cli_launch(
        &mut self,
        epoch: &str,
        id: &str,
        engine_id: &str,
        context: &crate::execution_context::ExecutionContext,
    ) -> Result<CliResources> {
        self.begin_cli_resources(epoch, id, engine_id, Some(context))
    }

    fn begin_cli_resources(
        &mut self,
        epoch: &str,
        id: &str,
        engine_id: &str,
        context: Option<&crate::execution_context::ExecutionContext>,
    ) -> Result<CliResources> {
        validate_engine_id(engine_id)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, true)?;
        let mut run: Run = load(&tx, "runs", id)?;
        if run.epoch != epoch || run.state != "prepared" || run.launch_started {
            return Err(Error::Conflict(
                "CLI 资源只能登记到本 epoch 尚未启动的 Run".into(),
            ));
        }
        crate::runs::check_run_authority(&tx, &run)?;
        let task: Task = load(&tx, "tasks", &run.task_id)?;
        if task.revision != run.task_revision {
            return Err(Error::Conflict("CLI 启动的任务版本已变化".into()));
        }
        let config: ExecutionConfiguration = load(&tx, "execution_configs", &run.configuration_id)?;
        let version: ConnectionVersion =
            load(&tx, "connection_versions", &config.connection_version)?;
        version.specification.validate()?;
        let ConnectionSpec::AgentCli {
            image: Some(image),
            egress_hosts,
            ..
        } = version.specification
        else {
            return Err(Error::Unavailable(
                "CLI 隔离资源需要冻结的 agent-cli 连接与固定镜像".into(),
            ));
        };
        if context.is_some() && egress_hosts.is_none() {
            return Err(Error::Unavailable(
                "CLI 冻结连接缺少显式出站策略，不能启动".into(),
            ));
        }
        if config.worker_id != run.worker_id {
            return Err(Error::Forbidden("CLI 执行配置不属于该成员".into()));
        }
        let native = context
            .map(|context| crate::execution_context::mark_used(&tx, &run, context))
            .transpose()?;
        let record = CliResources {
            run_id: run.id.clone(),
            workspace_id: tx.query_row("SELECT id FROM workspace", [], |r| r.get(0))?,
            ownership_token: new_id(),
            engine_id: engine_id.into(),
            execution_container: format!("atelier-cli-{}", run.id),
            proxy_container: format!("atelier-proxy-{}", run.id),
            internal_network: format!("atelier-inner-{}", run.id),
            egress_network: format!("atelier-outer-{}", run.id),
            image,
            egress_hosts: egress_hosts.unwrap_or_default(),
            context_id: native.map(|context| context.id),
            resume: context.map(|context| context.used),
            resources_stopped: false,
            diagnostic: None,
        };
        save(&tx, &record)?;
        run.launch_started = true;
        crate::runs::save(&tx, &run)?;
        tx.commit()?;
        Ok(record)
    }
}

// Fixed commands only; no Docker arguments, resource names or labels from a
// model. Suppress raw Docker stderr and bound the entire read/wait operation.
async fn docker(args: &[&str]) -> Result<Vec<u8>> {
    const LIMIT: u64 = 128 * 1024;
    let mut cmd = tokio::process::Command::new("docker");
    cmd.env_clear();
    for key in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
    ] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    let mut child = cmd
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Error::Unavailable("Docker 资源核对无法启动".into()))?;
    let mut output = child.stdout.take().expect("piped stdout").take(LIMIT + 1);
    let outcome = tokio::time::timeout(Duration::from_secs(15), async {
        let mut bytes = Vec::new();
        output.read_to_end(&mut bytes).await?;
        if bytes.len() > LIMIT as usize {
            return Err(Error::Unavailable("Docker 资源核对输出超限".into()));
        }
        if !child.wait().await?.success() {
            return Err(Error::Unavailable("Docker 资源核对未成功".into()));
        }
        Ok(bytes)
    })
    .await;
    match outcome {
        Ok(Ok(bytes)) => Ok(bytes),
        _ => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(Error::Unavailable("Docker 资源核对失败或超时".into()))
        }
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Container,
    Network,
}
impl Kind {
    fn noun(self) -> &'static str {
        match self {
            Self::Container => "container",
            Self::Network => "network",
        }
    }
    fn name_key(self) -> &'static str {
        match self {
            Self::Container => "Names",
            Self::Network => "Name",
        }
    }
}
fn object_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_engine_id(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-:".contains(&b))
    {
        return Err(Error::Invalid("Docker 引擎标识无效".into()));
    }
    Ok(())
}
/// Trusted preflight uses this identity in the durable intent before creation.
pub async fn engine_identity() -> Result<String> {
    let bytes = docker(&["info", "--format", "{{.ID}}"]).await?;
    let id = std::str::from_utf8(&bytes)
        .map_err(|_| Error::Unavailable("Docker 引擎标识无效".into()))?
        .trim();
    validate_engine_id(id)?;
    Ok(id.into())
}
async fn lookup(kind: Kind, name: &str) -> Result<Option<String>> {
    let filter = format!("name={name}");
    let mut args = vec![
        kind.noun(),
        "ls",
        "--no-trunc",
        "--filter",
        &filter,
        "--format",
        "{{json .}}",
    ];
    if matches!(kind, Kind::Container) {
        args.push("--all");
    }
    let bytes = docker(&args).await?;
    let mut found = None;
    for line in bytes.split(|b| *b == b'\n').filter(|b| !b.is_empty()) {
        let value: Value = serde_json::from_slice(line)?;
        let observed_name = value[kind.name_key()]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Unavailable("Docker 资源名称缺失".into()))?;
        let id = value["ID"]
            .as_str()
            .filter(|id| object_id(id))
            .ok_or_else(|| Error::Unavailable("Docker 资源标识无效".into()))?;
        if observed_name == name && found.replace(id.to_string()).is_some() {
            return Err(Error::Conflict("Docker 资源名称不唯一".into()));
        }
    }
    Ok(found)
}
fn owned(value: &Value, kind: Kind, name: &str, id: &str, record: &CliResources) -> bool {
    let labels = match kind {
        Kind::Container => &value["Config"]["Labels"],
        Kind::Network => &value["Labels"],
    };
    let expected_name = match kind {
        Kind::Container => format!("/{name}"),
        Kind::Network => name.to_string(),
    };
    value["Id"] == id
        && value["Name"] == expected_name
        && record
            .labels()
            .iter()
            .all(|(key, expected)| labels[key] == *expected)
}
async fn remove_owned(kind: Kind, name: &str, record: &CliResources) -> Result<()> {
    let Some(id) = lookup(kind, name).await? else {
        return Ok(());
    };
    let value: Value = serde_json::from_slice(&docker(&[kind.noun(), "inspect", &id]).await?)?;
    let object = value
        .as_array()
        .filter(|v| v.len() == 1)
        .and_then(|v| v.first())
        .ok_or_else(|| Error::Unavailable("Docker 资源检查结果无效".into()))?;
    if !owned(object, kind, name, &id, record) {
        return Err(Error::Conflict("同名 Docker 资源归属不符，未删除".into()));
    }
    if matches!(kind, Kind::Network)
        && !object["Containers"]
            .as_object()
            .is_some_and(|v| v.is_empty())
    {
        return Err(Error::Conflict("网络仍有挂接资源，未强制断开".into()));
    }
    // Delete by inspected immutable ID, never by a name that can be replaced.
    let args = match kind {
        Kind::Container => vec!["container", "rm", "--force", &id],
        Kind::Network => vec!["network", "rm", &id],
    };
    docker(&args).await?;
    if lookup(kind, name).await?.is_some() {
        return Err(Error::Conflict(
            "核对期间出现同名资源，继续保留运行占用".into(),
        ));
    }
    Ok(())
}
async fn cleanup(record: &CliResources) -> Result<()> {
    if engine_identity().await? != record.engine_id {
        return Err(Error::Conflict(
            "Docker 引擎已变化，不能用另一引擎的空列表证明旧资源停止".into(),
        ));
    }
    // Stop execution before egress. Never remove a shared login/session volume.
    for (kind, name) in [
        (Kind::Container, &record.execution_container),
        (Kind::Container, &record.proxy_container),
        (Kind::Network, &record.internal_network),
        (Kind::Network, &record.egress_network),
    ] {
        remove_owned(kind, name, record).await?;
    }
    Ok(())
}
/// Only recover after the owning adapter process group is absent, otherwise it
/// might still create resources after inspection. Missing PID remains unknown.
pub(crate) async fn recover(client: &DatabaseClient, epoch: &str) -> Result<usize> {
    let records = client.call(|store| {
        let mut stmt = store.connection.prepare("SELECT r.data,c.data FROM runs r JOIN cli_resources c ON c.run_id=r.id WHERE r.state='unknown'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?,r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter().map(|(run, record)| Ok((serde_json::from_str::<Run>(&run)?,serde_json::from_str::<CliResources>(&record)?))).collect::<Result<Vec<_>>>()
    }).await?;
    let mut reconciled = 0;
    for (run, mut record) in records {
        let absent = match run.pid {
            Some(pid) => crate::api_driver::group_absent(pid).await?,
            None => false,
        };
        let outcome = if !absent {
            Err(Error::Conflict(
                "CLI 接入进程组仍在或缺少启动登记，尚不能核对容器".into(),
            ))
        } else {
            cleanup(&record).await
        };
        record.resources_stopped = outcome.is_ok();
        record.diagnostic = Some(match &outcome {
            Ok(()) => "CLI 接入进程组、执行容器、代理和网络均已核对退出".into(),
            Err(e) => e.to_string(),
        });
        let owner = epoch.to_string();
        let saved = record.clone();
        client
            .call(move |store| {
                let tx = store
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                crate::runs::service(&tx, &owner, false)?;
                let current: Run = load(&tx, "runs", &saved.run_id)?;
                let prior = read(&tx, &saved.run_id)?
                    .ok_or_else(|| Error::Conflict("CLI 资源记录缺失".into()))?;
                if current.state != "unknown" || prior.ownership_token != saved.ownership_token {
                    return Err(Error::Conflict("CLI 资源核对依据已变化".into()));
                }
                save(&tx, &saved)?;
                tx.commit()?;
                Ok(())
            })
            .await?;
        if outcome.is_err() {
            continue;
        }
        let result = if matches!(run.purpose.as_str(), "execute" | "rework") {
            client
                .runtime_finish_execution(epoch.into(), run.id.clone(), "CLI 资源已核对停止".into())
                .await
                .map(|_| ())
        } else {
            let owner = epoch.to_string();
            let id = run.id.clone();
            client
                .call(move |store| {
                    store
                        .runtime_run_observed_stopped(&owner, &id, "CLI 资源已核对停止")
                        .map(|_| ())
                })
                .await
        };
        match result {
            Ok(()) => reconciled += 1,
            Err(Error::Database(e)) => return Err(Error::Database(e)),
            Err(_) => {} // Missing fixed output or live checks still own the Run.
        }
    }
    Ok(reconciled)
}
