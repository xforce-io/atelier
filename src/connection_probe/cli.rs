//! Worker-scoped diagnostics, with independent durable resource ownership.
//! No business Task, Run, mailbox message, or execution context is created.
use super::*;
use crate::{cli_environment::CliEnvironment, database::DatabaseClient};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Binding {
    configuration_id: String,
    login_generation: u64,
    image: String,
    engine_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Resources {
    workspace_id: String,
    ownership_token: String,
    engine_id: String,
    creation_authorized: bool,
    pid: Option<u32>,
    process_identity: Option<String>,
    pub stopped: bool,
    #[serde(default)]
    storage_owned: bool,
}
pub(super) fn binding(
    db: &rusqlite::Connection,
    version: &ConnectionVersion,
    worker: Option<&str>,
) -> Result<Option<Binding>> {
    let Some(worker) = worker else {
        return Ok(None);
    };
    let configuration = crate::content::digest(&serde_json::to_vec(&(worker, &version.id))?);
    let data: Option<String> = db
        .query_row(
            "SELECT data FROM cli_environments WHERE id=?1",
            [&configuration],
            |r| r.get(0),
        )
        .optional()?;
    data.map(|data| {
        let environment: CliEnvironment = serde_json::from_str(&data)?;
        Ok(Binding {
            configuration_id: configuration,
            login_generation: environment.login_generation,
            image: environment.image,
            engine_id: environment.engine_id,
        })
    })
    .transpose()
}

pub(crate) fn ensure_idle(db: &rusqlite::Connection, worker: &str) -> Result<()> {
    if db.query_row("SELECT EXISTS(SELECT 1 FROM connection_tests WHERE json_extract(data,'$.workerId')=?1 AND json_extract(data,'$.resources.stopped')=0)", [worker], |r|r.get::<_,bool>(0))? {
        return Err(Error::Conflict("成员还有未核对的 CLI 连接检查；按原 requestId 或 runtime reconcile 核对".into()));
    }
    Ok(())
}

pub(super) fn readiness(db: &rusqlite::Connection, connection: &ModelConnection) -> Result<Value> {
    let version: ConnectionVersion = load(db, "connection_versions", &connection.current_version)?;
    let mut stmt=db.prepare("SELECT w.data FROM workers w JOIN execution_configs c ON c.id=json_extract(w.data,'$.execution_config') WHERE json_extract(c.data,'$.connection_version')=?1 ORDER BY w.id")?;
    let workers = stmt
        .query_map([&version.id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut rows = Vec::new();
    for data in workers {
        let worker: Worker = serde_json::from_str(&data)?;
        let data:Option<String>=db.query_row("SELECT data FROM connection_tests WHERE json_extract(data,'$.connectionVersion')=?1 AND json_extract(data,'$.workerId')=?2 ORDER BY rowid DESC LIMIT 1",params![version.id,worker.id],|r|r.get(0)).optional()?;
        let mut state = "unchecked";
        let last = if let Some(data) = data {
            let record: Record = serde_json::from_str(&data)?;
            let current = binding(db, &version, Some(&worker.id))?;
            let applicable =
                record.cli == current && record.adapter_revision == crate::channel::MILKIE_COMMIT;
            let usable = if let Some(ref current) = current {
                let environment: CliEnvironment =
                    load(db, "cli_environments", &current.configuration_id)?;
                environment.state == "prepared" && environment.login_material_ready
            } else {
                false
            };
            if applicable {
                state = match record.state.as_str() {
                    "passed"
                        if usable
                            && record.cli.is_some()
                            && record.resources.as_ref().is_some_and(|r| r.stopped) =>
                    {
                        "ready"
                    }
                    "passed" => "unchecked",
                    "failed" | "unsupported" => "unavailable",
                    _ => "inconclusive",
                };
            }
            let mut last = serde_json::to_value(record)?;
            last["appliesToCurrentEnvironment"] = json!(applicable && (state != "unchecked"));
            last
        } else {
            Value::Null
        };
        rows.push(json!({"workerId":worker.id,"configurationId":worker.execution_config,"readiness":state,"lastTest":last}));
    }
    // A model connection is shared configuration, not a shared CLI login.
    Ok(
        json!({"readiness":"unchecked","readinessScope":"worker","lastTest":null,"workerReadiness":rows}),
    )
}

fn publish(store: &mut Store, record: &Record) -> Result<()> {
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let prior: Record = load(&tx, "connection_tests", &record.id)?;
    if prior.request_id != record.request_id
        || prior.cli != record.cli
        || prior.resources.as_ref().is_some_and(|r| r.stopped)
    {
        return Err(Error::Conflict("CLI 检查记录已变化".into()));
    }
    save(&tx, "connection_tests", &record.id, record)?;
    if tx.execute(
        "UPDATE requests SET result=?2 WHERE actor=(SELECT self_id FROM workspace) AND id=?1",
        params![record.request_id, serde_json::to_string(record)?],
    )? != 1
    {
        return Err(Error::Conflict("连接检查请求缺失".into()));
    }
    tx.commit()?;
    Ok(())
}
fn names(id: &str) -> [String; 4] {
    [
        format!("atelier-probe-{id}"),
        format!("atelier-probe-proxy-{id}"),
        format!("atelier-probe-inner-{id}"),
        format!("atelier-probe-outer-{id}"),
    ]
}
async fn cleanup(record: &Record, workspace: &Path) -> Result<()> {
    if let Some(resources) = &record.resources {
        if resources.creation_authorized {
            let pid = resources
                .pid
                .ok_or_else(|| Error::Conflict("检查缺少进程归属".into()))?;
            if !crate::api_driver::group_absent(pid).await? {
                return Err(Error::Conflict("检查创建进程组仍在运行".into()));
            }
            let labels = BTreeMap::from([
                ("atelier.workspace".into(), resources.workspace_id.clone()),
                ("atelier.probe".into(), record.id.clone()),
                ("atelier.owner".into(), resources.ownership_token.clone()),
            ]);
            let names = names(&record.id);
            crate::cli_resources::cleanup_named(
                &resources.engine_id,
                &labels,
                names.each_ref().map(String::as_str),
            )
            .await?;
        }
    }
    // All diagnostic native dialogue is transient. Shared login is a separate
    // mount and is never under this directory; remove_dir_all doesn't follow links.
    if !record.resources.as_ref().is_some_and(|r| r.storage_owned) {
        return Ok(());
    }
    let path = workspace.join("connection-probes").join(&record.id);
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn terminal(record: &mut Record, outcome: Outcome, stopped: bool) {
    if let Some(r) = &mut record.resources {
        r.stopped = stopped;
    }
    record.state = if stopped {
        outcome.state
    } else {
        "inconclusive".into()
    };
    record.code = if stopped {
        outcome.code
    } else {
        "resources_unresolved".into()
    };
    record.http_status = None;
    record.finished_at_ms = Some(now());
}
pub(super) async fn reconcile(store: &mut Store, mut record: Record) -> Result<Value> {
    let stopped = cleanup(&record, &store.workspace_path).await.is_ok();
    terminal(
        &mut record,
        Outcome::new("inconclusive", "probe_interrupted"),
        stopped,
    );
    publish(store, &record)?;
    Ok(serde_json::to_value(record)?)
}

pub(super) async fn test(store: &mut Store, preparation: Preparation) -> Result<Value> {
    let mut record = preparation.record;
    let outcome = perform(store, &mut record, &preparation.version)
        .await
        .unwrap_or_else(|_| Outcome::new("inconclusive", "adapter_failed"));
    let stopped = cleanup(&record, &store.workspace_path).await.is_ok();
    terminal(&mut record, outcome, stopped);
    publish(store, &record)?;
    Ok(serde_json::to_value(record)?)
}
async fn perform(
    store: &mut Store,
    record: &mut Record,
    version: &ConnectionVersion,
) -> Result<Outcome> {
    let Some(binding) = &record.cli else {
        return Ok(Outcome::new("failed", "cli_environment_unavailable"));
    };
    let environment = store
        .cli_environment(&binding.configuration_id)?
        .ok_or_else(|| Error::Unavailable("检查环境缺失".into()))?;
    if environment.state != "prepared" {
        return Ok(Outcome::new("failed", "cli_environment_unavailable"));
    }
    if !environment.login_material_ready
        || !crate::cli_login::login_material(&environment, &store.workspace_path)
    {
        return Ok(Outcome::new("failed", "cli_login_required"));
    }
    if environment.validate_storage(&store.workspace_path).is_err() {
        return Ok(Outcome::new("failed", "cli_environment_changed"));
    }
    let engine = crate::cli_resources::engine_identity().await?;
    if binding.engine_id.as_ref() != Some(&engine) {
        return Ok(Outcome::new("failed", "cli_environment_changed"));
    }
    let ConnectionSpec::AgentCli {
        runtime,
        model,
        egress_hosts: Some(hosts),
        egress_proxy,
        ..
    } = &version.specification
    else {
        return Ok(Outcome::new("failed", "cli_environment_unavailable"));
    };
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("adapters/milkie/dist/src/cli-probe-launcher.js");
    if !entry.is_file() {
        return Ok(Outcome::new("failed", "adapter_unavailable"));
    }
    let image = binding.image.clone();
    record.resources = Some(Resources {
        workspace_id: store.workspace()?["id"]
            .as_str()
            .ok_or_else(|| Error::Invalid("工作区标识缺失".into()))?
            .into(),
        ownership_token: new_id(),
        engine_id: engine.clone(),
        creation_authorized: false,
        pid: None,
        process_identity: None,
        stopped: false,
        storage_owned: false,
    });
    publish(store, record)?;
    let root = store.workspace_path.join("connection-probes");
    crate::execution_context::private_directory(&root, true)?;
    let directory = root.join(&record.id);
    // Never adopt a pre-existing diagnostic context.
    if directory.try_exists()? {
        return Err(Error::Conflict("检查上下文已存在".into()));
    }
    crate::execution_context::private_directory(&directory, true)?;
    record
        .resources
        .as_mut()
        .expect("registered resources")
        .storage_owned = true;
    publish(store, record)?;
    for name in ["native", "ledger", "skill"] {
        crate::execution_context::private_directory(&directory.join(name), true)?;
    }
    let resources = record.resources.as_ref().expect("registered resources");
    let mut payload = json!({"isolation":{"runId":record.id,"workspaceId":resources.workspace_id,"ownershipToken":resources.ownership_token,"engineId":engine,"image":image,"hosts":hosts,
        "nativeDirectory":directory.join("native"),"ledgerDirectory":directory.join("ledger"),"skillDirectory":directory.join("skill"),"configDirectory":environment.directory(&store.workspace_path).join("login")},
        "diagnostic":{"runtime":runtime,"model":model,"challenge":new_id()}});
    if let Some(proxy) = egress_proxy {
        payload["isolation"]["upstream"] = json!(proxy);
    }
    let mut command = tokio::process::Command::new("node");
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
    command
        .arg(entry)
        .arg("--atelier-probe")
        .arg(&record.id)
        .current_dir(&store.workspace_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let mut input = child.stdin.take().expect("private control pipe");
    let stdout = child.stdout.take().expect("private output");
    let result = async {
        let pid = child
            .id()
            .ok_or_else(|| Error::Unavailable("检查进程缺少标识".into()))?;
        let identity = crate::api_driver::process_identity(pid).await?;
        let resources = record.resources.as_mut().expect("registered resources");
        resources.pid = Some(pid);
        resources.process_identity = Some(identity);
        resources.creation_authorized = true;
        publish(store, record)?;
        let mut bytes = serde_json::to_vec(&payload)?;
        bytes.push(b'\n');
        input.write_all(&bytes).await?;
        let mut output = Vec::new();
        stdout.take(4097).read_to_end(&mut output).await?;
        if output.len() > 4096 {
            return Err(Error::Invalid("检查输出超限".into()));
        }
        let outcome: Outcome = serde_json::from_slice(&output)?;
        outcome.validate()?;
        if !matches!(
            outcome.code.as_str(),
            "cli_tool_roundtrip"
                | "cli_tool_unverified"
                | "cli_native_failed"
                | "cancelled"
                | "deadline"
                | "adapter_failed"
        ) {
            return Err(Error::Invalid("CLI 检查回执类型无效".into()));
        }
        Ok::<_, Error>(outcome)
    };
    let mut outcome = match tokio::time::timeout(Duration::from_secs(150), result).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => Outcome::new("inconclusive", "adapter_failed"),
        Err(_) => Outcome::new("inconclusive", "deadline"),
    };
    drop(input);
    match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(Ok(status)) if status.success() => {}
        _ => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            outcome = Outcome::new("inconclusive", "adapter_failed");
        }
    }
    Ok(outcome)
}

pub(crate) async fn recover(client: &DatabaseClient, workspace: &Path) -> Result<usize> {
    let pending=client.call(|store|Ok(store.connection.query_row("SELECT EXISTS(SELECT 1 FROM connection_tests WHERE json_extract(data,'$.resources.stopped')=0)",[],|r|r.get::<_,bool>(0))?)).await?;
    if !pending {
        return Ok(0);
    }
    let path = workspace.to_path_buf();
    let _lock = match tokio::task::spawn_blocking(move || crate::cli_environment::lock(&path))
        .await
        .map_err(|_| Error::Unavailable("检查核对线程退出".into()))?
    {
        Ok(lock) => lock,
        Err(Error::Conflict(_)) => return Ok(0),
        Err(e) => return Err(e),
    };
    let records=client.call(|store| {
        let mut stmt=store.connection.prepare("SELECT data FROM connection_tests WHERE json_extract(data,'$.resources.stopped')=0")?;
        let rows=stmt.query_map([],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        rows.into_iter().map(|s|serde_json::from_str::<Record>(&s).map_err(Into::into)).collect::<Result<Vec<_>>>()
    }).await?;
    let mut count = 0;
    for mut record in records {
        let stopped = cleanup(&record, workspace).await.is_ok();
        if !stopped && record.code == "resources_unresolved" {
            continue;
        }
        terminal(
            &mut record,
            Outcome::new("inconclusive", "probe_interrupted"),
            stopped,
        );
        client.call(move |store| publish(store, &record)).await?;
        count += usize::from(stopped);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Store, Command, CliEnvironment, Worker) {
        let dir = tempfile::tempdir().unwrap();
        Store::init(dir.path(), "检查测试").unwrap();
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
                        egress_proxy: None,
                    },
                },
            )
            .unwrap();
        let id = connection["connection"]["id"].as_str().unwrap().to_string();
        let worker: Worker = serde_json::from_value(
            store
                .execute(
                    "worker",
                    &Command::WorkerCreate {
                        name: "成员".into(),
                        description: String::new(),
                        connection: Some(id.clone()),
                    },
                )
                .unwrap(),
        )
        .unwrap();
        let other: Worker = serde_json::from_value(
            store
                .execute(
                    "other",
                    &Command::WorkerCreate {
                        name: "同连接另一成员".into(),
                        description: String::new(),
                        connection: Some(id.clone()),
                    },
                )
                .unwrap(),
        )
        .unwrap();
        // Metadata-only fixture: never opens a real login or engine.
        let environment:CliEnvironment=serde_json::from_value(json!({"id":worker.execution_config,"workerId":worker.id,"connectionVersion":connection["connection"]["current_version"],"runtime":"pi","image":format!("sha256:{}","a".repeat(64)),"engineId":"test-engine","state":"prepared","code":"fixture","loginGeneration":7,"loginMaterialReady":true,"everPrepared":true,"preparationRequestId":"fixture"})).unwrap();
        save(
            &store.connection,
            "cli_environments",
            &environment.id,
            &environment,
        )
        .unwrap();
        (
            dir,
            store,
            Command::ConnectionTest {
                id,
                revision: 1,
                version: None,
                worker: Some(worker.id),
            },
            environment,
            other,
        )
    }
    fn resources(stopped: bool) -> Resources {
        Resources {
            workspace_id: new_id(),
            ownership_token: new_id(),
            engine_id: "fixture".into(),
            creation_authorized: false,
            pid: None,
            process_identity: None,
            stopped,
            storage_owned: false,
        }
    }
    fn state(store: &Store, command: &Command, worker: &str) -> Value {
        let Command::ConnectionTest { id, .. } = command else {
            unreachable!()
        };
        let view = store.connection_view(id).unwrap();
        assert_eq!(view["readiness"], "unchecked");
        assert_eq!(view["readinessScope"], "worker");
        view["workerReadiness"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["workerId"] == worker)
            .unwrap()
            .clone()
    }
    #[test]
    fn cli_readiness_is_per_worker_and_invalidated_by_login_image_engine_and_adapter() {
        let (_dir, mut store, command, mut environment, other) = fixture();
        let (mut record, _) = store.prepare_connection_test("probe", &command).unwrap();
        record.resources = Some(resources(true));
        terminal(
            &mut record,
            Outcome::new("passed", "cli_tool_roundtrip"),
            true,
        );
        publish(&mut store, &record).unwrap();
        assert_eq!(
            state(&store, &command, &environment.worker_id)["readiness"],
            "ready"
        );
        assert_eq!(state(&store, &command, &other.id)["readiness"], "unchecked");
        store
            .execute(
                "description",
                &Command::WorkerUpdate {
                    id: environment.worker_id.clone(),
                    revision: 1,
                    name: None,
                    description: Some("新的说明".into()),
                    connection: None,
                    clear_connection: false,
                },
            )
            .unwrap();
        assert_eq!(
            state(&store, &command, &environment.worker_id)["readiness"],
            "ready"
        );
        let original = environment.clone();
        for change in 0..4 {
            environment = original.clone();
            match change {
                0 => environment.login_generation += 1,
                1 => environment.image = format!("sha256:{}", "b".repeat(64)),
                2 => environment.engine_id = Some("another-engine".into()),
                _ => environment.login_material_ready = false,
            };
            save(
                &store.connection,
                "cli_environments",
                &environment.id,
                &environment,
            )
            .unwrap();
            assert_eq!(
                state(&store, &command, &environment.worker_id)["readiness"],
                "unchecked"
            );
        }
        save(
            &store.connection,
            "cli_environments",
            &original.id,
            &original,
        )
        .unwrap();
        record.adapter_revision = "old-adapter".into();
        save(&store.connection, "connection_tests", &record.id, &record).unwrap();
        assert_eq!(
            state(&store, &command, &environment.worker_id)["readiness"],
            "unchecked"
        );
    }
    #[tokio::test]
    async fn interrupted_diagnostic_only_reconciles_and_blocks_new_environment_use() {
        let (_dir, mut store, command, environment, _) = fixture();
        let (mut record, _) = store.prepare_connection_test("probe", &command).unwrap();
        record.resources = Some(resources(false));
        publish(&mut store, &record).unwrap();
        assert!(ensure_idle(&store.connection, &environment.worker_id).is_err());
        assert!(store.prepare_connection_test("another", &command).is_err());
        let result = super::super::test(&mut store, "probe", &command)
            .await
            .unwrap();
        assert_eq!(result["id"], record.id);
        assert_eq!(result["code"], "probe_interrupted");
        assert_eq!(result["resources"]["stopped"], true);
        ensure_idle(&store.connection, &environment.worker_id).unwrap();
        assert_eq!(
            super::super::test(&mut store, "probe", &command)
                .await
                .unwrap(),
            result
        );
        assert_eq!(store.request("probe").unwrap(), result);
        for table in [
            "tasks",
            "runs",
            "messages",
            "deliveries",
            "execution_contexts",
        ] {
            let count: i64 = store
                .connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
    }
    #[test]
    fn failed_publication_keeps_unresolved_resources_and_does_not_publish_success() {
        let (_dir, mut store, command, environment, _) = fixture();
        let (mut record, _) = store.prepare_connection_test("probe", &command).unwrap();
        record.resources = Some(resources(false));
        publish(&mut store, &record).unwrap();
        store.connection.execute_batch("CREATE TRIGGER fail_probe_publish BEFORE UPDATE ON requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        terminal(
            &mut record,
            Outcome::new("passed", "cli_tool_roundtrip"),
            true,
        );
        assert!(publish(&mut store, &record).is_err());
        assert!(ensure_idle(&store.connection, &environment.worker_id).is_err());
        assert_eq!(store.request("probe").unwrap()["state"], "pending");
        assert_eq!(
            state(&store, &command, &environment.worker_id)["readiness"],
            "inconclusive"
        );
    }

    #[tokio::test]
    async fn completed_replay_does_not_require_an_idle_environment() {
        let (directory, mut store, command, _, _) = fixture();
        let (mut record, _) = store.prepare_connection_test("probe", &command).unwrap();
        record.resources = Some(resources(true));
        terminal(
            &mut record,
            Outcome::new("passed", "cli_tool_roundtrip"),
            true,
        );
        publish(&mut store, &record).unwrap();
        let _other_owner = crate::cli_environment::lock(directory.path()).unwrap();
        let result = super::super::test(&mut store, "probe", &command)
            .await
            .unwrap();
        assert_eq!(result["code"], "cli_tool_roundtrip");
        assert!(
            super::super::test(&mut store, "new-probe", &command)
                .await
                .is_err()
        );
    }
}
