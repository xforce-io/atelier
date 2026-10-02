//! Bounded, explicit connection diagnostics. The durable request is reserved
//! before I/O; replay never silently starts another probe.
use crate::{
    Error, Result,
    connection::{ConnectionSpec, ConnectionVersion, ExecutionConfiguration, ModelConnection},
    credential::CredentialReference,
    model::{Command, Worker, WorkerKind},
    store::{Store, load, new_id, revision, save, text},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    id: String,
    request_id: String,
    connection_id: String,
    connection_version: String,
    credential_generation: Option<u64>,
    worker_id: Option<String>,
    adapter_revision: String,
    state: String,
    code: String,
    http_status: Option<u16>,
    started_at_ms: u64,
    finished_at_ms: Option<u64>,
}
struct Preparation {
    record: Record,
    version: ConnectionVersion,
    reference: Option<CredentialReference>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Outcome {
    state: String,
    code: String,
    http_status: Option<u16>,
}
impl Outcome {
    fn new(state: &str, code: &str) -> Self {
        Self {
            state: state.into(),
            code: code.into(),
            http_status: None,
        }
    }
    fn validate(&self) -> Result<()> {
        let valid = match (self.state.as_str(), self.code.as_str()) {
            ("passed", "response_received") => self.http_status.is_none(),
            ("failed", "provider_rejected") => {
                self.http_status.is_some_and(|s| (400..=599).contains(&s))
            }
            (
                "failed",
                "connection_failed"
                | "credential_missing"
                | "credential_unavailable"
                | "adapter_unavailable",
            )
            | ("inconclusive", "invalid_response" | "deadline" | "cancelled" | "adapter_failed")
            | ("unsupported", "agent_cli_unavailable") => self.http_status.is_none(),
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(Error::Invalid("连接检查结果无效".into()))
        }
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl Store {
    fn prepare_connection_test(
        &mut self,
        request_id: &str,
        command: &Command,
    ) -> Result<(Record, Option<Preparation>)> {
        text(request_id, "requestId", 128)?;
        let Command::ConnectionTest {
            id,
            revision: expected,
            version,
            worker,
        } = command
        else {
            return Err(Error::Invalid("不是连接检查请求".into()));
        };
        let fingerprint = crate::content::digest(&serde_json::to_vec(command)?);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let actor: String = tx.query_row("SELECT self_id FROM workspace", [], |r| r.get(0))?;
        if tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_requests WHERE request_id=?1)",
            [request_id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Conflict("requestId 已用于凭据操作".into()));
        }
        let prior: Option<(String, String)> = tx
            .query_row(
                "SELECT fingerprint,result FROM requests WHERE actor=?1 AND id=?2",
                params![actor, request_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((hash, result)) = prior {
            if hash != fingerprint {
                return Err(Error::Conflict("同 requestId 的内容不同".into()));
            }
            return Ok((serde_json::from_str(&result)?, None));
        }
        let connection: ModelConnection = load(&tx, "connections", id)?;
        revision(connection.revision, *expected)?;
        let selected: ConnectionVersion = load(
            &tx,
            "connection_versions",
            version.as_deref().unwrap_or(&connection.current_version),
        )?;
        if selected.connection_id != *id {
            return Err(Error::Forbidden("检查版本不属于该连接".into()));
        }
        selected.specification.validate()?;
        match &selected.specification {
            ConnectionSpec::Api { .. } if worker.is_some() => {
                return Err(Error::Invalid("API 连接检查不指定 Worker".into()));
            }
            ConnectionSpec::AgentCli { .. } => {
                let worker = worker.as_deref().ok_or_else(|| {
                    Error::Invalid("CLI 连接检查须指定 --worker 的专用环境".into())
                })?;
                let member: Worker = load(&tx, "workers", worker)?;
                if member.kind != WorkerKind::Agent {
                    return Err(Error::Invalid("CLI 检查须指定数字员工".into()));
                }
                let config: ExecutionConfiguration = load(
                    &tx,
                    "execution_configs",
                    member
                        .execution_config
                        .as_deref()
                        .ok_or_else(|| Error::Invalid("成员未绑定执行配置".into()))?,
                )?;
                if config.connection_version != selected.id || config.worker_id != member.id {
                    return Err(Error::Conflict("成员专用环境与检查连接版本不符".into()));
                }
            }
            _ => {}
        }
        let reference: Option<String> = tx
            .query_row(
                "SELECT data FROM credentials WHERE version_id=?1",
                [&selected.id],
                |r| r.get(0),
            )
            .optional()?;
        let reference: Option<CredentialReference> =
            reference.map(|s| serde_json::from_str(&s)).transpose()?;
        let record = Record {
            id: new_id(),
            request_id: request_id.into(),
            connection_id: id.clone(),
            connection_version: selected.id.clone(),
            credential_generation: reference.as_ref().map(|r| r.generation),
            worker_id: worker.clone(),
            adapter_revision: crate::channel::MILKIE_COMMIT.into(),
            state: "pending".into(),
            code: "outcome_pending".into(),
            http_status: None,
            started_at_ms: now(),
            finished_at_ms: None,
        };
        save(&tx, "connection_tests", &record.id, &record)?;
        tx.execute(
            "INSERT INTO requests(actor,id,fingerprint,command,result) VALUES(?1,?2,?3,?4,?5)",
            params![
                actor,
                request_id,
                fingerprint,
                serde_json::to_string(command)?,
                serde_json::to_string(&record)?
            ],
        )?;
        tx.commit()?;
        Ok((
            record.clone(),
            Some(Preparation {
                record,
                version: selected,
                reference,
            }),
        ))
    }

    fn finish_connection_test(&mut self, id: &str, outcome: Outcome) -> Result<Record> {
        outcome.validate()?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut record: Record = load(&tx, "connection_tests", id)?;
        if record.state != "pending" {
            return Ok(record);
        }
        record.state = outcome.state;
        record.code = outcome.code;
        record.http_status = outcome.http_status;
        record.finished_at_ms = Some(now());
        save(&tx, "connection_tests", id, &record)?;
        let updated = tx.execute(
            "UPDATE requests SET result=?2 WHERE actor=(SELECT self_id FROM workspace) AND id=?1",
            params![record.request_id, serde_json::to_string(&record)?],
        )?;
        if updated != 1 {
            return Err(Error::Conflict("连接检查请求记录缺失".into()));
        }
        tx.commit()?;
        Ok(record)
    }

    pub(crate) fn connection_readiness(&self, connection: &ModelConnection) -> Result<Value> {
        readiness(&self.connection, connection)
    }
}

pub(crate) fn readiness(db: &rusqlite::Connection, connection: &ModelConnection) -> Result<Value> {
    let data: Option<String> = db.query_row("SELECT data FROM connection_tests WHERE json_extract(data,'$.connectionId')=?1 ORDER BY CASE WHEN json_extract(data,'$.connectionVersion')=?2 THEN 0 ELSE 1 END,rowid DESC LIMIT 1", params![connection.id, connection.current_version], |r|r.get(0)).optional()?;
    let Some(data) = data else {
        return Ok(json!({"readiness":"unchecked","lastTest":null}));
    };
    let record: Record = serde_json::from_str(&data)?;
    let generation: Option<u64> = db
        .query_row(
            "SELECT json_extract(data,'$.generation') FROM credentials WHERE version_id=?1",
            [&connection.current_version],
            |r| r.get(0),
        )
        .optional()?;
    let applicable = record.connection_version == connection.current_version
        && record.credential_generation == generation
        && record.adapter_revision == crate::channel::MILKIE_COMMIT;
    let readiness = if !applicable {
        "unchecked"
    } else {
        match record.state.as_str() {
            "passed" => "ready",
            "failed" | "unsupported" => "unavailable",
            _ => "inconclusive",
        }
    };
    let mut last = serde_json::to_value(record)?;
    last["appliesToCurrentConnection"] = json!(applicable);
    Ok(json!({"readiness":readiness,"lastTest":last}))
}

/// Explicit diagnostic only. Does not create business objects, alter model
/// permissions, or silently turn a cached pending result into another request.
pub async fn test(store: &mut Store, request_id: &str, command: &Command) -> Result<Value> {
    let (record, preparation) = store.prepare_connection_test(request_id, command)?;
    let Some(preparation) = preparation else {
        return Ok(serde_json::to_value(record)?);
    };
    let id = preparation.record.id.clone();
    let outcome = observe(preparation).await;
    Ok(serde_json::to_value(
        store.finish_connection_test(&id, outcome)?,
    )?)
}

async fn observe(preparation: Preparation) -> Outcome {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(29);
    let ConnectionSpec::Api {
        protocol,
        model,
        base_url,
    } = preparation.version.specification
    else {
        return Outcome::new("unsupported", "agent_cli_unavailable");
    };
    let Some(reference) = preparation.reference else {
        return Outcome::new("failed", "credential_missing");
    };
    let secret = match tokio::time::timeout_at(
        deadline,
        tokio::task::spawn_blocking(move || crate::credential::load_secret(&reference)),
    )
    .await
    {
        Ok(Ok(Ok(secret))) => secret,
        Err(_) => return Outcome::new("inconclusive", "deadline"),
        _ => return Outcome::new("failed", "credential_unavailable"),
    };
    let mut payload = json!({"protocol":protocol,"model":model,"apiKey":secret});
    if let Some(url) = base_url {
        payload["baseUrl"] = json!(url);
    }
    let entry =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("adapters/milkie/dist/src/probe-main.js");
    if !entry.is_file() {
        return Outcome::new("failed", "adapter_unavailable");
    }
    let mut command = tokio::process::Command::new("node");
    command
        .arg(entry)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("LOG_LEVEL", "silent")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return Outcome::new("failed", "adapter_unavailable"),
    };
    let mut input = child.stdin.take().expect("piped stdin");
    let output = child.stdout.take().expect("piped stdout");
    let response = tokio::time::timeout_at(deadline, async {
        let mut bytes = serde_json::to_vec(&payload)?;
        bytes.push(b'\n');
        input.write_all(&bytes).await?;
        let mut result = Vec::new();
        output.take(4097).read_to_end(&mut result).await?;
        if result.len() > 4096 {
            return Err(Error::Invalid("连接检查输出超限".into()));
        }
        let outcome: Outcome = serde_json::from_slice(&result)?;
        outcome.validate()?;
        Ok::<_, Error>(outcome)
    })
    .await;
    drop(input);
    let (outcome, stop) = match response {
        Ok(Ok(outcome)) => (outcome, false),
        Ok(Err(_)) => (Outcome::new("inconclusive", "invalid_response"), true),
        Err(_) => (Outcome::new("inconclusive", "deadline"), true),
    };
    if stop {
        let _ = child.start_kill();
    }
    match tokio::time::timeout(Duration::from_millis(500), child.wait()).await {
        Ok(Ok(status)) if status.success() || stop => outcome,
        _ => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            Outcome::new("inconclusive", "adapter_failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::ApiProtocol;
    fn fixture() -> (tempfile::TempDir, Store, String, Command) {
        let directory = tempfile::tempdir().unwrap();
        Store::init(directory.path(), "本人").unwrap();
        let mut store = Store::open(directory.path()).unwrap();
        let saved = store
            .execute(
                "connection",
                &Command::ConnectionCreate {
                    name: "合成连接".into(),
                    specification: ConnectionSpec::Api {
                        protocol: ApiProtocol::OpenaiChatCompletions,
                        model: "fixture".into(),
                        base_url: Some("https://example.invalid".into()),
                    },
                },
            )
            .unwrap();
        let id = saved["connection"]["id"].as_str().unwrap().to_string();
        let command = Command::ConnectionTest {
            id: id.clone(),
            revision: 1,
            version: None,
            worker: None,
        };
        (directory, store, id, command)
    }
    fn no_business(store: &Store) {
        for table in ["tasks", "runs", "messages", "deliveries", "workers"] {
            let n: u64 = store
                .connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, u64::from(table == "workers"));
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn missing_credential_is_persisted_and_replay_never_starts_another_probe() {
        let (_directory, mut store, id, command) = fixture();
        let result = test(&mut store, "probe", &command).await.unwrap();
        assert_eq!(result["state"], "failed");
        assert_eq!(result["code"], "credential_missing");
        assert_eq!(
            store.connection_view(&id).unwrap()["readiness"],
            "unavailable"
        );
        assert_eq!(store.request("probe").unwrap(), result);
        assert_eq!(test(&mut store, "probe", &command).await.unwrap(), result);
        no_business(&store);
        assert!(
            store
                .execute(
                    "probe",
                    &Command::ConnectionCreate {
                        name: "collision".into(),
                        specification: ConnectionSpec::Api {
                            protocol: ApiProtocol::OpenaiChatCompletions,
                            model: "fixture".into(),
                            base_url: None
                        }
                    }
                )
                .is_err()
        );
        assert!(
            store
                .set_credential("probe", &id, 1, "synthetic-never-stored")
                .is_err()
        );
    }
    #[test]
    fn readiness_binds_version_and_credential_generation_not_member_description() {
        let (_directory, mut store, id, command) = fixture();
        let version = store.connection_view(&id).unwrap()["version"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let reference = CredentialReference {
            connection_version: version.clone(),
            generation: 1,
            account: "metadata-only-fixture".into(),
        };
        store
            .connection
            .execute(
                "INSERT INTO credentials(version_id,data) VALUES(?1,?2)",
                params![version, serde_json::to_string(&reference).unwrap()],
            )
            .unwrap();
        let (record, _) = store.prepare_connection_test("probe", &command).unwrap();
        store
            .finish_connection_test(&record.id, Outcome::new("passed", "response_received"))
            .unwrap();
        assert_eq!(store.connection_view(&id).unwrap()["readiness"], "ready");
        let worker = store
            .execute(
                "worker",
                &Command::WorkerCreate {
                    connection: Some(id.clone()),
                    name: "成员".into(),
                    description: "原说明".into(),
                },
            )
            .unwrap();
        store
            .execute(
                "description",
                &Command::WorkerUpdate {
                    id: worker["id"].as_str().unwrap().into(),
                    revision: 1,
                    name: None,
                    description: Some("新的工作说明".into()),
                    connection: None,
                    clear_connection: false,
                },
            )
            .unwrap();
        assert_eq!(store.connection_view(&id).unwrap()["readiness"], "ready");
        let mut replacement = reference;
        replacement.generation = 2;
        store
            .connection
            .execute(
                "UPDATE credentials SET data=?2 WHERE version_id=?1",
                params![version, serde_json::to_string(&replacement).unwrap()],
            )
            .unwrap();
        let stale = store.connection_view(&id).unwrap();
        assert_eq!(stale["readiness"], "unchecked");
        assert_eq!(stale["lastTest"]["state"], "passed");
        assert_eq!(stale["lastTest"]["appliesToCurrentConnection"], false);
        let (newer, _) = store
            .prepare_connection_test("probe-new", &command)
            .unwrap();
        store
            .finish_connection_test(&newer.id, Outcome::new("failed", "connection_failed"))
            .unwrap();
        assert_eq!(
            store.connection_view(&id).unwrap()["readiness"],
            "unavailable"
        );
        assert!(
            store
                .prepare_connection_test("probe", &command)
                .unwrap()
                .1
                .is_none()
        );
        store
            .execute(
                "change-model",
                &Command::ConnectionUpdate {
                    id: id.clone(),
                    revision: 1,
                    name: None,
                    specification: Some(ConnectionSpec::Api {
                        protocol: ApiProtocol::OpenaiChatCompletions,
                        model: "other-model".into(),
                        base_url: Some("https://example.invalid".into()),
                    }),
                },
            )
            .unwrap();
        assert_eq!(
            store.connection_view(&id).unwrap()["readiness"],
            "unchecked"
        );
        let latest = Command::ConnectionTest {
            id: id.clone(),
            revision: 2,
            version: None,
            worker: None,
        };
        let (current, _) = store
            .prepare_connection_test("current-probe", &latest)
            .unwrap();
        store
            .finish_connection_test(&current.id, Outcome::new("failed", "credential_missing"))
            .unwrap();
        let historical = Command::ConnectionTest {
            id: id.clone(),
            revision: 2,
            version: Some(version),
            worker: None,
        };
        let (old, _) = store
            .prepare_connection_test("old-version-probe", &historical)
            .unwrap();
        store
            .finish_connection_test(&old.id, Outcome::new("passed", "response_received"))
            .unwrap();
        assert_eq!(
            store.connection_view(&id).unwrap()["lastTest"]["id"],
            current.id,
            "旧版本的后续检查不覆盖当前版本诊断"
        );
    }
    #[test]
    fn probe_reservation_and_result_publication_are_atomic_and_lost_response_stays_pending() {
        let (directory, mut store, _id, command) = fixture();
        store.connection.execute_batch("CREATE TRIGGER fail_request BEFORE INSERT ON requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(store.prepare_connection_test("probe", &command).is_err());
        let n: u64 = store
            .connection
            .query_row("SELECT count(*) FROM connection_tests", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        store
            .connection
            .execute_batch("DROP TRIGGER fail_request;")
            .unwrap();
        let (record, plan) = store.prepare_connection_test("probe", &command).unwrap();
        assert!(plan.is_some());
        let (replayed, plan) = Store::open(directory.path())
            .unwrap()
            .prepare_connection_test("probe", &command)
            .unwrap();
        assert!(plan.is_none());
        assert_eq!(replayed.id, record.id);
        assert_eq!(replayed.state, "pending");
        store.connection.execute_batch("CREATE TRIGGER fail_result BEFORE UPDATE ON requests BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
        assert!(
            store
                .finish_connection_test(&record.id, Outcome::new("passed", "response_received"))
                .is_err()
        );
        let saved: Record = load(&store.connection, "connection_tests", &record.id).unwrap();
        assert_eq!(saved.state, "pending");
        assert_eq!(store.request("probe").unwrap()["state"], "pending");
        store
            .connection
            .execute_batch("DROP TRIGGER fail_result;")
            .unwrap();
        store
            .finish_connection_test(&record.id, Outcome::new("passed", "response_received"))
            .unwrap();
        assert_eq!(store.request("probe").unwrap()["state"], "passed");
        assert!(
            store
                .finish_connection_test(&record.id, Outcome::new("passed", "credential_missing"))
                .is_err()
        );
        no_business(&store);
    }
}
