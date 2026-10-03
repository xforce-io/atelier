use crate::{
    Error, Result,
    content::{self, GitInput},
    model::*,
    profile::ProfileRecord,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

const DATABASE: &str = "atelier.sqlite3";

pub struct Store {
    pub(crate) connection: Connection,
    self_id: String,
    pub(crate) workspace_path: PathBuf,
}

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn text(value: &str, label: &str, max: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max {
        return Err(Error::Invalid(format!(
            "{label}不能为空且须不超过 {max} 字节"
        )));
    }
    Ok(())
}

pub(crate) fn revision(actual: u64, expected: u64) -> Result<()> {
    if actual != expected {
        return Err(Error::Conflict(format!(
            "版本已变化，当前为 {actual}，请求为 {expected}"
        )));
    }
    Ok(())
}

// Table names are fixed by callers in this module, never accepted from a CLI request.
pub(crate) fn load<T: DeserializeOwned>(db: &Connection, table: &str, id: &str) -> Result<T> {
    let value: Option<String> = db
        .query_row(
            &format!("SELECT data FROM {table} WHERE id=?1"),
            [id],
            |r| r.get(0),
        )
        .optional()?;
    serde_json::from_str(&value.ok_or_else(|| Error::NotFound(format!("对象不存在：{id}")))?)
        .map_err(Into::into)
}

pub(crate) fn save<T: Serialize>(db: &Connection, table: &str, id: &str, value: &T) -> Result<()> {
    db.execute(&format!("INSERT INTO {table}(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data"),
        params![id, serde_json::to_string(value)?])?;
    Ok(())
}

pub(crate) fn immutable<T: Serialize>(
    db: &Connection,
    table: &str,
    id: &str,
    value: &T,
) -> Result<()> {
    let data = serde_json::to_string(value)?;
    let prior: Option<String> = db
        .query_row(
            &format!("SELECT data FROM {table} WHERE id=?1"),
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(prior) = prior {
        if prior != data {
            return Err(Error::Conflict("固定记录不允许覆盖".into()));
        }
    } else {
        db.execute(
            &format!("INSERT INTO {table}(id,data) VALUES(?1,?2)"),
            params![id, data],
        )?;
    }
    Ok(())
}

fn validate_contract_refs(db: &Connection, contract: &Contract, accepting: bool) -> Result<()> {
    if accepting && contract.code_input.is_some() != contract.verification_profile.is_some() {
        return Err(Error::Invalid(
            "代码任务承接需要同时绑定固定输入和检验配置".into(),
        ));
    }
    if let Some(id) = &contract.code_input {
        let _: GitInput = load(db, "inputs", id)?;
    }
    if let Some(id) = &contract.verification_profile {
        let profile: ProfileRecord = load(db, "profiles", id)?;
        if profile.specification.record()? != profile {
            return Err(Error::Conflict("检验配置摘要不匹配".into()));
        }
    }
    Ok(())
}

pub(crate) fn save_task(db: &Connection, task: &Task) -> Result<()> {
    db.execute("INSERT INTO tasks(id,team_id,state,data) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET state=excluded.state,data=excluded.data",
        params![task.id,task.team_id,task.state,serde_json::to_string(task)?])?;
    Ok(())
}

impl Store {
    pub fn init(path: &Path, name: &str) -> Result<Value> {
        text(name, "本人名称", 256)?;
        if path.exists() {
            if fs::symlink_metadata(path)?.file_type().is_symlink()
                || !path.is_dir()
                || fs::read_dir(path)?.next().is_some()
            {
                return Err(Error::Conflict(
                    "初始化要求新的或已存在的空目录，不覆盖已有工作区".into(),
                ));
            }
        } else {
            fs::create_dir_all(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
            }
        }
        // Exclusive file creation prevents concurrent initializers from replacing each other.
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path.join(DATABASE))?;
        drop(file);
        let mut db = Self::connect(path)?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(include_str!("schema.sql"))?;
        let worker = Worker {
            execution_config: None,
            id: new_id(),
            name: name.into(),
            kind: WorkerKind::Human,
            description: String::new(),
            revision: 1,
        };
        save(&tx, "workers", &worker.id, &worker)?;
        tx.execute(
            "INSERT INTO workspace(id,self_id) VALUES(?1,?2)",
            params![new_id(), worker.id],
        )?;
        tx.commit()?;
        Self::open(path)?.workspace()
    }

    fn connect(path: &Path) -> Result<Connection> {
        let file = path.join(DATABASE);
        let metadata = fs::symlink_metadata(&file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::NotFound("工作区不存在；请先 workspace init".into())
            } else {
                Error::Io(e)
            }
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::Invalid("数据库必须是工作区中的普通文件".into()));
        }
        let db = Connection::open_with_flags(
            file,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(Duration::from_secs(2))?;
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "synchronous", "FULL")?;
        Ok(db)
    }

    pub fn open(path: &Path) -> Result<Self> {
        let db = Self::connect(path)?;
        let version: i64 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version != 23 {
            return Err(Error::Invalid(format!(
                "工作区格式不支持或初始化未完成：{version}；请保留原目录修复"
            )));
        }
        let self_id: String = db.query_row("SELECT self_id FROM workspace", [], |r| r.get(0))?;
        let worker: Worker = load(&db, "workers", &self_id)?;
        if worker.kind != WorkerKind::Human {
            return Err(Error::Invalid("工作区本人身份损坏".into()));
        }
        Ok(Self {
            workspace_path: fs::canonicalize(path)?,
            connection: db,
            self_id,
        })
    }

    pub fn workspace(&self) -> Result<Value> {
        let id: String = self
            .connection
            .query_row("SELECT id FROM workspace", [], |r| r.get(0))?;
        Ok(
            json!({"id": id,"self": self.worker(&self.self_id)?,"schemaVersion":23,
            "capabilities":{"persistentCore":true,"runtimeLifecycle":true,"execution":true,"executionTransports":["api"]}}),
        )
    }

    pub fn worker(&self, id: &str) -> Result<Worker> {
        load(&self.connection, "workers", id)
    }
    pub fn team(&self, id: &str) -> Result<Team> {
        load(&self.connection, "teams", id)
    }
    pub fn task(&self, id: &str) -> Result<Task> {
        load(&self.connection, "tasks", id)
    }

    pub fn task_details(&self, id: &str) -> Result<Value> {
        let tx = self.connection.unchecked_transaction()?;
        let task: Task = load(&tx, "tasks", id)?;
        let mut value = json!(task);
        value["blockers"] = crate::blocker::list(&tx, id)?;
        value["reworks_reserved"] = json!(crate::rework::reserved(&tx, id)?);
        value["rework_arrangements"] = crate::rework::list(&tx, id)?;
        tx.commit()?;
        Ok(value)
    }

    pub fn input(&self, id: &str) -> Result<GitInput> {
        load(&self.connection, "inputs", id)
    }
    pub fn profile(&self, id: &str) -> Result<ProfileRecord> {
        load(&self.connection, "profiles", id)
    }

    pub fn decision(&self, id: &str) -> Result<DecisionRequest> {
        load(&self.connection, "decisions", id)
    }
    pub fn decisions(&self, task_id: &str) -> Result<Value> {
        self.task(task_id)?;
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM decisions WHERE task_id=?1 ORDER BY rowid")?;
        let rows = stmt
            .query_map([task_id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!(
            rows.iter()
                .map(|s| serde_json::from_str::<Value>(s))
                .collect::<std::result::Result<Vec<_>, _>>()?
        ))
    }

    pub fn connection_version(&self, id: &str) -> Result<crate::connection::ConnectionVersion> {
        load(&self.connection, "connection_versions", id)
    }
    pub fn connection_view(&self, id: &str) -> Result<Value> {
        let tx = self.connection.unchecked_transaction()?;
        let connection: crate::connection::ModelConnection =
            load(&self.connection, "connections", id)?;
        let mut value = self.connection_readiness(&connection)?;
        value["version"] =
            serde_json::to_value(self.connection_version(&connection.current_version)?)?;
        value["credentialGeneration"] = json!(
            self.credential_reference(&connection.current_version)?
                .map(|c| c.generation)
        );
        value["executionSupported"] = json!(matches!(
            self.connection_version(&connection.current_version)?
                .specification,
            crate::connection::ConnectionSpec::Api { .. }
        ));
        value["connection"] = serde_json::to_value(connection)?;
        let mut stmt=tx.prepare("SELECT e.data FROM cli_environments e JOIN connection_versions v ON v.id=json_extract(e.data,'$.connectionVersion') WHERE json_extract(v.data,'$.connection_id')=?1 ORDER BY e.id")?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        value["cliEnvironments"] = json!(
            rows.into_iter()
                .map(|s| serde_json::from_str::<Value>(&s))
                .collect::<std::result::Result<Vec<_>, _>>()?
        );
        drop(stmt);
        let mut stmt=tx.prepare("SELECT l.data FROM cli_logins l JOIN cli_environments e ON e.id=json_extract(l.data,'$.environmentId') JOIN connection_versions v ON v.id=json_extract(e.data,'$.connectionVersion') WHERE json_extract(v.data,'$.connection_id')=?1 ORDER BY l.rowid")?;
        let rows = stmt
            .query_map([id], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        value["cliLogins"] = json!(
            rows.into_iter()
                .map(|s| serde_json::from_str::<Value>(&s))
                .collect::<std::result::Result<Vec<_>, _>>()?
        );
        drop(stmt);
        tx.commit()?;
        Ok(value)
    }
    pub fn execution_configuration(
        &self,
        id: &str,
    ) -> Result<crate::connection::ExecutionConfiguration> {
        load(&self.connection, "execution_configs", id)
    }

    pub fn list(&self, kind: &str) -> Result<Value> {
        let table = match kind {
            "connection" => "connections",
            "input" => "inputs",
            "profile" => "profiles",
            "worker" => "workers",
            "team" => "teams",
            "task" => "tasks",
            _ => return Err(Error::Invalid("未知对象类别".into())),
        };
        let mut stmt = self
            .connection
            .prepare(&format!("SELECT data FROM {table} ORDER BY rowid"))?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let values = rows
            .iter()
            .map(|s| serde_json::from_str::<Value>(s))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(json!(values))
    }

    pub fn mailbox(&self, receiver: Option<&str>) -> Result<Value> {
        let receiver = receiver.unwrap_or(&self.self_id);
        self.worker(receiver)?;
        let mut stmt = self.connection.prepare("SELECT d.id,d.revision,d.status,d.reason,m.id,m.task_id,m.sender,m.recipient,m.kind,m.body,m.reply_to,m.causation_id,d.run_id,d.claim_epoch,d.attempts,m.source,h.data,m.task_revision FROM deliveries d JOIN messages m ON m.id=d.message_id LEFT JOIN message_dispositions h ON h.delivery_id=d.id WHERE d.receiver=?1 ORDER BY m.rowid")?;
        let rows = stmt.query_map([receiver], |r| Ok(json!({
            "id":r.get::<_,String>(0)?,"revision":r.get::<_,u64>(1)?,"status":r.get::<_,String>(2)?,"reason":r.get::<_,Option<String>>(3)?,"runId":r.get::<_,Option<String>>(12)?,"claimEpoch":r.get::<_,Option<String>>(13)?,"attempts":r.get::<_,u64>(14)?,"handlingResult":r.get::<_,Option<String>>(16)?.map(|v|serde_json::from_str::<Value>(&v)).transpose().map_err(|error|rusqlite::Error::FromSqlConversionFailure(16,rusqlite::types::Type::Text,Box::new(error)))?,
            "message":{"taskRevision":r.get::<_,u64>(17)?,"id":r.get::<_,String>(4)?,"taskId":r.get::<_,String>(5)?,"sender":r.get::<_,Option<String>>(6)?,"source":r.get::<_,String>(15)?,"recipient":r.get::<_,String>(7)?,"kind":r.get::<_,String>(8)?,"body":r.get::<_,String>(9)?,"replyTo":r.get::<_,Option<String>>(10)?,"causationId":r.get::<_,String>(11)?}
        })))?.collect::<std::result::Result<Vec<_>,_>>()?;
        Ok(json!(rows))
    }

    pub fn request(&self, id: &str) -> Result<Value> {
        // Permission and cached-result reads use one snapshot, including concurrent revocation.
        let tx = self.connection.unchecked_transaction()?;
        let record: Option<(String, String)> = tx
            .query_row(
                "SELECT command,result FROM requests WHERE actor=?1 AND id=?2",
                params![self.self_id, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (command, result) =
            record.ok_or_else(|| Error::NotFound("请求未提交或不存在".into()))?;
        authorize(&tx, &self.self_id, &serde_json::from_str(&command)?)?;
        let result = serde_json::from_str(&result)?;
        tx.commit()?;
        Ok(result)
    }

    /// Runtime-only entry: caller must hold the workspace's exclusive service lock.
    pub fn runtime_register(&mut self, epoch: &str, pid: u32) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::recover_on_start(&tx, epoch)?;
        tx.execute("INSERT INTO runtime(singleton,epoch,pid,state,stop_requested) VALUES(1,?1,?2,'running',0) ON CONFLICT(singleton) DO UPDATE SET epoch=excluded.epoch,pid=excluded.pid,state='running',stop_requested=0",params![epoch,pid])?;
        tx.commit()?;
        Ok(())
    }

    pub fn runtime_snapshot(&self) -> Result<Value> {
        let record:Option<Value>=self.connection.query_row("SELECT epoch,pid,state,stop_requested FROM runtime WHERE singleton=1",[],|r|Ok(json!({"epoch":r.get::<_,String>(0)?,"pid":r.get::<_,u32>(1)?,"state":r.get::<_,String>(2)?,"stopRequested":r.get::<_,bool>(3)?}))).optional()?;
        let queued: u64 = self.connection.query_row(
            "SELECT count(*) FROM deliveries WHERE status='queued'",
            [],
            |r| r.get(0),
        )?;
        let mut value = record.unwrap_or_else(|| json!({"state":"not_started"}));
        value["queuedDeliveries"] = json!(queued);
        value["activeRuns"] = json!(crate::runs::active_count(&self.connection)?);
        value["executionSupported"] = json!(true);
        value["executionTransports"] = json!(["api"]);
        value["executionReason"] =
            json!("API 按冻结连接及本地凭据启动；agent CLI 隔离执行尚未接入");
        Ok(value)
    }

    pub fn runtime_request_stop(&mut self, epoch: &str) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE runtime SET stop_requested=1 WHERE singleton=1 AND epoch=?1",
            [epoch],
        )? != 1
        {
            return Err(Error::Conflict("服务 epoch 已变化，请重新查询".into()));
        }
        crate::runs::request_all_stop(&tx, "运行服务请求停止")?;
        tx.commit()?;
        Ok(())
    }

    pub fn runtime_should_stop(&self, epoch: &str) -> Result<bool> {
        let row: Option<bool> = self
            .connection
            .query_row(
                "SELECT stop_requested FROM runtime WHERE singleton=1 AND epoch=?1",
                [epoch],
                |r| r.get(0),
            )
            .optional()?;
        row.ok_or_else(|| Error::Conflict("运行服务 epoch 已失效".into()))
    }

    pub fn runtime_stopped(&mut self, epoch: &str) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if crate::runs::active_count(&tx)? > 0 {
            return Err(Error::Conflict(
                "Run 资源尚未确认停止，不能声明服务已停止".into(),
            ));
        }
        if tx.execute(
            "UPDATE runtime SET state='stopped',stop_requested=1 WHERE singleton=1 AND epoch=?1",
            [epoch],
        )? != 1
        {
            return Err(Error::Conflict("运行服务 epoch 已失效".into()));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn execute(&mut self, request_id: &str, command: &Command) -> Result<Value> {
        if matches!(
            command,
            Command::CredentialSet { .. }
                | Command::CredentialClear { .. }
                | Command::ConnectionTest { .. }
                | Command::ConnectionPrepare { .. }
                | Command::ConnectionLogin { .. }
        ) {
            return Err(Error::Invalid(
                "凭据管理和连接检查只能经专用入口执行".into(),
            ));
        }
        if self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM credential_requests WHERE request_id=?1)",
            [request_id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Conflict("requestId 已用于凭据操作".into()));
        }
        text(request_id, "requestId", 128)?;
        let fingerprint = format!("{:x}", Sha256::digest(serde_json::to_vec(command)?));
        // Replays bypass external preparation, but never current authorization.
        authorize(&self.connection, &self.self_id, command)?;
        let cached: Option<String> = self
            .connection
            .query_row(
                "SELECT fingerprint FROM requests WHERE actor=?1 AND id=?2",
                params![self.self_id, request_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(hash) = cached {
            if hash != fingerprint {
                return Err(Error::Conflict("同 requestId 的内容不同".into()));
            }
            return self.request(request_id);
        }
        // File IO happens before the write transaction. A failed publication can
        // leave unreferenced blobs; no task or request can refer to partial input.
        let prepared_input = match command {
            Command::InputImport { repository, commit } => Some(content::import_git(
                &self.workspace_path,
                Path::new(repository),
                commit,
            )?),
            _ => None,
        };
        if let Command::Intake {
            id,
            revision: expected,
            decision: IntakeDecision::Accept,
            ..
        } = command
        {
            let task = self.task(id)?;
            if task.revision == *expected {
                if let Some(id) = &task.contract.code_input {
                    content::validate_input(&self.workspace_path, &self.input(id)?)?;
                }
            }
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        authorize(&tx, &self.self_id, command)?;
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
                params![self.self_id, request_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((hash, result)) = prior {
            if hash != fingerprint {
                return Err(Error::Conflict("同 requestId 的内容不同".into()));
            }
            return Ok(serde_json::from_str(&result)?);
        }
        let result = match command {
            Command::InputImport { .. } => {
                let input =
                    prepared_input.ok_or_else(|| Error::Invalid("输入准备未完成".into()))?;
                immutable(&tx, "inputs", &input.id, &input)?;
                json!(input)
            }
            Command::ProfileImport { specification } => {
                let profile = specification.record()?;
                immutable(&tx, "profiles", &profile.id, &profile)?;
                json!(profile)
            }
            _ => apply(&tx, &self.self_id, request_id, command)?,
        };
        tx.execute(
            "INSERT INTO requests(actor,id,fingerprint,command,result) VALUES(?1,?2,?3,?4,?5)",
            params![
                self.self_id,
                request_id,
                fingerprint,
                serde_json::to_string(command)?,
                serde_json::to_string(&result)?
            ],
        )?;
        tx.commit()?;
        Ok(result)
    }
}

pub(crate) fn require(team: &Team, actor: &str, permission: Permission) -> Result<()> {
    if !team.members.iter().any(|id| id == actor)
        || !team
            .grants
            .get(actor)
            .is_some_and(|p| p.contains(&permission))
    {
        return Err(Error::Forbidden("当前成员不具备所需授权".into()));
    }
    Ok(())
}

fn authorize(db: &Connection, actor: &str, command: &Command) -> Result<()> {
    let task_permission = match command {
        Command::RecoveryApply { id, .. } => {
            return crate::recovery::authorize(db, actor, id).map(|_| ());
        }
        Command::MailboxRetry { id, .. } => {
            return crate::retry::authorize(db, actor, id).map(|_| ());
        }
        Command::AcceptanceRequest { task_id, .. } => {
            return crate::acceptance::authorize_request(
                db,
                &load::<Task>(db, "tasks", task_id)?,
                actor,
            );
        }
        Command::AcceptanceDecide { task_id, .. } => {
            return crate::acceptance::authorize_decision(
                db,
                &load::<Task>(db, "tasks", task_id)?,
                actor,
            );
        }
        Command::BlockerResolve { id, .. } => {
            let b: crate::blocker::Blocker = load(db, "blockers", id)?;
            let task: Task = load(db, "tasks", &b.task_id)?;
            return crate::blocker::authorize_resolve(db, &task, &b, actor);
        }

        Command::DecisionRequest { task_id, .. } => Some((task_id, Permission::Communicate)),
        Command::DecisionRespond { id, .. } | Command::DecisionRecord { id, .. } => {
            let decision: DecisionRequest = load(db, "decisions", id)?;
            if decision.kind == "recovery" {
                return crate::recovery::authorize(db, actor, id).map(|_| ());
            }
            let identity = if matches!(command, Command::DecisionRespond { .. }) {
                &decision.handler
            } else {
                &decision.requester
            };
            if identity != actor {
                return Err(Error::Forbidden(
                    "只能由指定处理者回应、原发起者落实决定".into(),
                ));
            }
            let task: Task = load(db, "tasks", &decision.task_id)?;
            return require(
                &load::<Team>(db, "teams", &task.team_id)?,
                actor,
                Permission::Communicate,
            );
        }
        Command::PermissionsUpdate { team_id, .. } => {
            return require(
                &load::<Team>(db, "teams", team_id)?,
                actor,
                Permission::Manage,
            );
        }
        Command::TeamUpdate { patch } => {
            return require(
                &load::<Team>(db, "teams", &patch.id)?,
                actor,
                Permission::Manage,
            );
        }
        Command::TaskCreate { team_id, .. } => {
            return require(
                &load::<Team>(db, "teams", team_id)?,
                actor,
                Permission::Communicate,
            );
        }
        Command::TaskUpdate { id, .. } | Command::TaskCancel { id, .. } => {
            Some((id, Permission::Manage))
        }
        Command::Intake { id, .. }
        | Command::TaskExecute { id, .. }
        | Command::TaskRework { id, .. }
        | Command::TaskVerify { id, .. } => Some((id, Permission::Arrange)),
        Command::MessageSend { task_id, .. } => Some((task_id, Permission::Communicate)),
        Command::MailboxRespond { id, .. } => {
            let (receiver,task_id):(String,String)=db.query_row("SELECT d.receiver,m.task_id FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE d.id=?1",[id],|r| Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or_else(||Error::NotFound("投递不存在".into()))?;
            if receiver != actor {
                return Err(Error::Forbidden("只能处理本人收件箱".into()));
            }
            let task: Task = load(db, "tasks", &task_id)?;
            return require(
                &load::<Team>(db, "teams", &task.team_id)?,
                actor,
                Permission::Communicate,
            );
        }
        _ => None,
    };
    if let Some((id, permission)) = task_permission {
        let task: Task = load(db, "tasks", id)?;
        if task.cancellation_requested && !matches!(command, Command::TaskCancel { .. }) {
            return Err(Error::Conflict("任务已请求取消".into()));
        }
        let team: Team = load(db, "teams", &task.team_id)?;
        require(&team, actor, permission)?;
        if matches!(
            command,
            Command::TaskExecute { .. } | Command::TaskVerify { .. } | Command::TaskRework { .. }
        ) {
            require(&team, actor, Permission::Communicate)?;
            require(&task.team_snapshot, actor, Permission::Arrange)?;
            require(&task.team_snapshot, actor, Permission::Communicate)?;
        }
        if matches!(
            command,
            Command::Intake { .. }
                | Command::TaskExecute { .. }
                | Command::TaskVerify { .. }
                | Command::TaskRework { .. }
        ) && actor != task.team_snapshot.leader
        {
            return Err(Error::Forbidden(
                "必须由该任务的团队负责人承接或安排；管理身份不能冒充".into(),
            ));
        }
        if matches!(command, Command::MessageSend { .. }) && !participants(&task).contains(&actor) {
            return Err(Error::Forbidden("只能在本人参与的任务中发送消息".into()));
        }
    }
    Ok(())
}

fn snapshot_workers(
    db: &Connection,
    team: &Team,
) -> Result<std::collections::BTreeMap<String, Worker>> {
    team.members
        .iter()
        .map(|id| Ok((id.clone(), load(db, "workers", id)?)))
        .collect()
}

pub(crate) fn participants(task: &Task) -> Vec<&str> {
    let t = &task.team_snapshot;
    let mut members = vec![t.leader.as_str(), t.acceptor.as_str()];
    if task.state != "pending" {
        members.extend(t.executor.as_deref());
        members.extend(t.verifier.as_deref());
    }
    members
}

fn validate_team(db: &Connection, team: &Team, actor: &str) -> Result<()> {
    text(&team.name, "团队名称", 256)?;
    if team.members.len() > 256 || team.members.is_empty() {
        return Err(Error::Invalid("团队成员数须在 1–256 内".into()));
    }
    let mut ids = team.members.clone();
    ids.sort();
    ids.dedup();
    if ids.len() != team.members.len() {
        return Err(Error::Invalid("成员重复".into()));
    }
    for member in &team.members {
        let _: Worker = load(db, "workers", member)?;
    }
    for id in [
        Some(&team.leader),
        Some(&team.acceptor),
        team.executor.as_ref(),
        team.verifier.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if !team.members.contains(id) {
            return Err(Error::Invalid("职责成员必须属于团队".into()));
        }
    }
    if team.acceptor != actor {
        return Err(Error::Invalid("首版验收者必须为本机本人".into()));
    }
    if team.executor.is_some() && team.executor == team.verifier {
        return Err(Error::Invalid("执行者与检验者必须独立".into()));
    }
    for worker in team.grants.keys() {
        if !team.members.contains(worker) {
            return Err(Error::Invalid("不能向团队外成员授权".into()));
        }
    }
    require(team, actor, Permission::Manage)?;
    Ok(())
}

pub(crate) struct Message<'a> {
    pub(crate) recipient: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) body: &'a str,
    pub(crate) reply_to: Option<&'a str>,
    pub(crate) event: Option<&'a str>,
    pub(crate) mandatory: bool,
}

pub(crate) fn enqueue(
    db: &Connection,
    task: &mut Task,
    actor: &str,
    cause: &str,
    message: Message<'_>,
) -> Result<Value> {
    enqueue_with_source(db, task, Some(actor), cause, message)
}

pub(crate) fn enqueue_system(
    db: &Connection,
    task: &mut Task,
    cause: &str,
    message: Message<'_>,
) -> Result<Value> {
    enqueue_with_source(db, task, None, cause, message)
}

fn enqueue_with_source(
    db: &Connection,
    task: &mut Task,
    actor: Option<&str>,
    cause: &str,
    message: Message<'_>,
) -> Result<Value> {
    if !message.mandatory && task.messages_used >= task.contract.max_messages {
        return Err(Error::Conflict("任务消息额度耗尽".into()));
    }
    let id = new_id();
    let delivery = new_id();
    let source = if matches!(message.kind, "work.note" | "work.question") {
        "worker"
    } else {
        "core"
    };
    db.execute("INSERT INTO messages(id,task_id,sender,recipient,kind,body,reply_to,causation_id,event_key,task_revision,source) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![id,task.id,actor,message.recipient,message.kind,message.body,message.reply_to,cause,message.event,task.revision,source])?;
    db.execute(
        "INSERT INTO deliveries(id,message_id,receiver,status) VALUES(?1,?2,?3,'queued')",
        params![delivery, id, message.recipient],
    )?;
    if !message.mandatory {
        task.messages_used += 1;
    }
    save_task(db, task)?;
    Ok(json!({"messageId":id,"deliveryId":delivery,"status":"queued"}))
}

pub(crate) fn apply(db: &Connection, actor: &str, cause: &str, command: &Command) -> Result<Value> {
    match command {
        Command::RecoveryApply { id, revision } => {
            crate::recovery::apply(db, actor, cause, id, *revision)
        }
        Command::MailboxRetry {
            id,
            revision,
            reason,
        } => crate::retry::apply(db, actor, cause, id, *revision, reason),
        Command::AcceptanceRequest { .. } | Command::AcceptanceDecide { .. } => {
            crate::acceptance::apply(db, actor, cause, command)
        }
        Command::BlockerResolve {
            id,
            revision,
            task_revision,
            evidence,
        } => crate::blocker::resolve(db, actor, cause, id, *revision, *task_revision, evidence),
        Command::CredentialSet { .. }
        | Command::CredentialClear { .. }
        | Command::ConnectionTest { .. }
        | Command::ConnectionPrepare { .. }
        | Command::ConnectionLogin { .. } => {
            Err(Error::Invalid("凭据操作不能通过普通业务命令执行".into()))
        }
        Command::TaskRework {
            id,
            revision,
            reason,
            instruction,
        } => crate::rework::arrange(db, actor, cause, id, *revision, reason, instruction),
        Command::TaskVerify {
            blocker_id,
            verification_id,
            id,
            revision,
            artifact_id,
            instruction,
        } => {
            if blocker_id.is_some() || verification_id.is_some() {
                crate::handoff::offer_again(db, actor, cause, command)
            } else {
                crate::handoff::offer(db, actor, cause, id, *revision, artifact_id, instruction)
            }
        }
        Command::TaskExecute {
            id,
            revision,
            instruction,
        } => crate::assignment::execute(db, actor, cause, id, *revision, instruction),
        Command::DecisionRequest { .. }
        | Command::DecisionRespond { .. }
        | Command::DecisionRecord { .. } => crate::decisions::apply(db, actor, cause, command),
        Command::InputImport { .. } | Command::ProfileImport { .. } => {
            Err(Error::Invalid("资源导入必须先经过准备".into()))
        }
        Command::ConnectionCreate { .. } | Command::ConnectionUpdate { .. } => {
            crate::connection::apply(db, command)
        }
        Command::WorkerCreate {
            name,
            description,
            connection,
        } => {
            text(name, "成员名称", 256)?;
            if description.len() > 65536 {
                return Err(Error::Invalid("工作说明过长".into()));
            }
            let mut worker = Worker {
                execution_config: None,
                id: new_id(),
                kind: WorkerKind::Agent,
                name: name.clone(),
                description: description.clone(),
                revision: 1,
            };
            save(db, "workers", &worker.id, &worker)?;
            if let Some(connection) = connection {
                crate::connection::bind_worker(db, &mut worker, connection)?;
                save(db, "workers", &worker.id, &worker)?;
            }
            Ok(json!(worker))
        }
        Command::WorkerUpdate {
            connection,
            clear_connection,
            id,
            revision: expected,
            name,
            description,
        } => {
            let mut worker: Worker = load(db, "workers", id)?;
            revision(worker.revision, *expected)?;
            if *clear_connection && connection.is_some() {
                return Err(Error::Invalid("不能同时设置与清除执行配置".into()));
            }
            if *clear_connection {
                if worker.kind != WorkerKind::Agent {
                    return Err(Error::Forbidden("人类成员不配置执行器".into()));
                }
                worker.execution_config = None;
            }
            if let Some(connection) = connection {
                crate::connection::bind_worker(db, &mut worker, connection)?;
            }
            if let Some(name) = name {
                text(name, "成员名称", 256)?;
                worker.name = name.clone();
            }
            if let Some(description) = description {
                if description.len() > 65536 {
                    return Err(Error::Invalid("工作说明过长".into()));
                }
                worker.description = description.clone();
            }
            worker.revision += 1;
            save(db, "workers", id, &worker)?;
            Ok(json!(worker))
        }
        Command::TeamCreate { team } => {
            let mut team = team.clone();
            team.id = new_id();
            team.revision = 1;
            team.authorization_revision = 1;
            let permissions = team.grants.entry(actor.into()).or_default();
            for p in [
                Permission::Manage,
                Permission::Arrange,
                Permission::Communicate,
                Permission::Accept,
            ] {
                if !permissions.contains(&p) {
                    permissions.push(p);
                }
            }
            validate_team(db, &team, actor)?;
            save(db, "teams", &team.id, &team)?;
            Ok(json!(team))
        }
        Command::TeamUpdate { patch } => {
            let mut team: Team = load(db, "teams", &patch.id)?;
            let old_grants = team.grants.clone();
            revision(team.revision, patch.revision)?;
            let active: i64 = db.query_row(
                "SELECT count(*) FROM tasks WHERE team_id=?1 AND state!='closed'",
                [&team.id],
                |r| r.get(0),
            )?;
            if team.leader != patch.leader && active > 0 {
                return Err(Error::Conflict("存在未闭合任务，不能更换团队负责人".into()));
            }
            team.name = patch.name.clone();
            team.members = patch.members.clone();
            team.leader = patch.leader.clone();
            if patch.executor.is_some() {
                team.executor = patch.executor.clone();
            }
            if patch.verifier.is_some() {
                team.verifier = patch.verifier.clone();
            }
            for (id, grants) in &patch.grants {
                let permissions = team.grants.entry(id.clone()).or_default();
                for p in grants {
                    if !permissions.contains(p) {
                        permissions.push(p.clone());
                    }
                }
            }
            team.revision += 1;
            if team.grants != old_grants {
                team.authorization_revision += 1;
            }
            validate_team(db, &team, actor)?;
            save(db, "teams", &team.id, &team)?;
            Ok(json!(team))
        }
        Command::PermissionsUpdate {
            decision_id,
            team_id,
            revision: expected,
            grant,
            revoke,
        } => {
            if grant.is_empty() && revoke.is_empty() {
                return Err(Error::Invalid("须明确提供授予或撤销项".into()));
            }
            let mut team: Team = load(db, "teams", team_id)?;
            revision(team.revision, *expected)?;
            if let Some(decision_id) = decision_id {
                crate::decisions::bind_change(db, decision_id, actor, cause, None, Some(team_id))?;
            }
            for (id, permissions) in grant.iter().chain(revoke.iter()) {
                if !team.members.contains(id) {
                    return Err(Error::Invalid("授权对象必须为当前团队成员".into()));
                }
                if permissions.is_empty() {
                    return Err(Error::Invalid("权限列表不能为空".into()));
                }
            }
            for (id, permissions) in grant {
                if revoke
                    .get(id)
                    .is_some_and(|removed| permissions.iter().any(|p| removed.contains(p)))
                {
                    return Err(Error::Invalid("同一权限不能同时授予和撤销".into()));
                }
                let target = team.grants.entry(id.clone()).or_default();
                for p in permissions {
                    if !target.contains(p) {
                        target.push(p.clone());
                    }
                }
            }
            for (id, permissions) in revoke {
                if let Some(target) = team.grants.get_mut(id) {
                    target.retain(|p| !permissions.contains(p));
                }
            }
            team.revision += 1;
            team.authorization_revision += 1;
            save(db, "teams", team_id, &team)?;
            let mut affected = 0;
            for (worker, permissions) in revoke {
                let kinds: Vec<&str> = permissions
                    .iter()
                    .flat_map(|p| match p {
                        Permission::Arrange => vec![
                            "intake",
                            "intake.updated",
                            "work.note",
                            "work.question",
                            "result",
                            "failure",
                            "blocker",
                            "resolved",
                            "decision.request",
                            "decision.result",
                        ],
                        Permission::Execute => vec!["assignment.execute", "assignment.rework"],
                        Permission::Verify => vec!["handoff.verify"],
                        Permission::Communicate => vec![
                            "intake",
                            "intake.updated",
                            "work.note",
                            "work.question",
                            "result",
                            "failure",
                            "blocker",
                            "resolved",
                            "decision.request",
                            "decision.result",
                            "assignment.execute",
                            "assignment.rework",
                            "handoff.verify",
                        ],
                        _ => vec![],
                    })
                    .collect();
                for kind in kinds {
                    affected+=db.execute("UPDATE deliveries SET status='blocked',reason='成员所需权限已撤销',revision=revision+1 WHERE receiver=?1 AND status='queued' AND message_id IN (SELECT m.id FROM messages m JOIN tasks t ON t.id=m.task_id WHERE t.team_id=?2 AND m.kind=?3)",params![worker,team_id,kind])?;
                }
            }
            crate::recovery::revoked(db, team_id, cause, revoke)?;
            let stopped_runs = crate::runs::stop_revoked(db, team_id, revoke)?;
            Ok(json!({"team":team,"blockedDeliveries":affected,"activeRuns":stopped_runs}))
        }
        Command::TaskCreate { team_id, goal } => {
            text(goal, "目标", 65536)?;
            let team: Team = load(db, "teams", team_id)?;
            let recipient = team.leader.clone();
            let mut task = Task {
                current_artifact: None,
                id: new_id(),
                goal: goal.clone(),
                team_id: team_id.clone(),
                worker_snapshots: snapshot_workers(db, &team)?,
                team_snapshot: team,
                state: "pending".into(),
                cancellation_requested: false,
                outcome: None,
                owner: None,
                revision: 1,
                contract: Contract::default(),
                messages_used: 0,
                runs_used: 0,
                reworks_used: 0,
            };
            save_task(db, &task)?;
            let message = enqueue(
                db,
                &mut task,
                actor,
                cause,
                Message {
                    recipient: &recipient,
                    kind: "intake",
                    body: goal,
                    reply_to: None,
                    event: None,
                    mandatory: false,
                },
            )?;
            Ok(json!({"task":task,"delivery":message}))
        }
        Command::TaskUpdate {
            decision_id,
            id,
            revision: expected,
            goal,
            contract,
            refresh_team,
        } => {
            let mut task: Task = load(db, "tasks", id)?;
            revision(task.revision, *expected)?;
            if task.state != "pending" {
                return Err(Error::Conflict("仅 pending 任务允许补齐".into()));
            }
            let goal = goal.as_ref().unwrap_or(&task.goal).clone();
            text(&goal, "目标", 65536)?;
            let contract = contract.merge(&task.contract);
            validate_contract_refs(db, &contract, false)?;
            let continuing = load::<Worker>(db, "workers", actor)?.kind == WorkerKind::Agent;
            if contract.max_runs == 0
                || contract.max_runs > task.contract.max_runs
                || contract.max_messages == 0
                || contract.max_messages > task.contract.max_messages
                || contract.max_reworks > task.contract.max_reworks
                || contract.max_messages < task.messages_used
                || (!continuing && contract.max_messages == task.messages_used)
                || contract.max_runs < task.runs_used
                || contract.max_reworks < task.reworks_used
            {
                return Err(Error::Invalid(
                    "额度只能收紧，且须容纳已用额度及本次更新消息".into(),
                ));
            }
            for input in [&contract.inputs, &contract.delivery, &contract.verification] {
                if input.len() > 65536 {
                    return Err(Error::Invalid("契约字段过长".into()));
                }
            }
            if *refresh_team {
                if crate::runs::active_for_task(db, id)? > 0 {
                    return Err(Error::Conflict(
                        "活动或未知 Run 核对前不能刷新任务配置".into(),
                    ));
                }
                let team: Team = load(db, "teams", &task.team_id)?;
                if team.leader != task.team_snapshot.leader {
                    return Err(Error::Conflict("不能替换任务团队负责人".into()));
                }
                task.worker_snapshots = snapshot_workers(db, &team)?;
                task.team_snapshot = team;
            }
            if let Some(decision_id) = decision_id {
                crate::decisions::bind_change(db, decision_id, actor, cause, Some(id), None)?;
            }
            task.goal = goal.clone();
            task.contract = contract;
            task.revision += 1;
            crate::decisions::supersede(
                db,
                id,
                decision_id.as_deref(),
                task.revision,
                "任务依据已更新",
            )?;
            crate::blocker::supersede(db, id, "任务版本已更新")?;
            db.execute("UPDATE deliveries SET status='cancelled',reason='任务版本已更新',revision=revision+1 WHERE message_id IN (SELECT id FROM messages WHERE task_id=?1 AND kind IN ('intake','intake.updated')) AND status IN ('queued','blocked')",[id])?;
            if continuing {
                // The bound member is already processing this task. Keep its
                // receipt as the continuation instead of creating a duplicate.
                save_task(db, &task)?;
                return Ok(json!({"task":task,"delivery":null}));
            }
            let recipient = task.team_snapshot.leader.clone();
            let delivery = enqueue(
                db,
                &mut task,
                actor,
                cause,
                Message {
                    recipient: &recipient,
                    kind: "intake.updated",
                    body: &goal,
                    reply_to: None,
                    event: None,
                    mandatory: false,
                },
            )?;
            Ok(json!({"task":task,"delivery":delivery}))
        }
        Command::Intake {
            id,
            revision: expected,
            decision,
            reason,
        } => {
            let mut task: Task = load(db, "tasks", id)?;
            revision(task.revision, *expected)?;
            if task.state != "pending" {
                return Err(Error::Conflict("任务已经承接或关闭".into()));
            }
            text(reason, "决定依据", 65536)?;
            match decision {
                IntakeDecision::Accept => {
                    crate::blocker::ensure_clear(db, &task)?;
                    validate_contract_refs(db, &task.contract, true)?;
                    text(&task.contract.inputs, "输入与前置条件", 65536)?;
                    text(&task.contract.delivery, "交付要求", 65536)?;
                    text(&task.contract.verification, "检验方式", 65536)?;
                    if task.team_snapshot.executor.is_none()
                        || task.team_snapshot.verifier.is_none()
                    {
                        return Err(Error::Invalid("需明确执行与独立检验成员".into()));
                    }
                    task.state = "active".into();
                    task.owner = Some(actor.into());
                }
                IntakeDecision::Wait => {}
                IntakeDecision::Decline => {
                    task.state = "closed".into();
                    task.outcome = Some("declined".into());
                }
            }
            task.revision += 1;
            crate::decisions::supersede(db, id, None, task.revision, "承接依据已变化")?;
            let actor_worker: Worker = load(db, "workers", actor)?;
            let human = actor_worker.kind == WorkerKind::Human;
            db.execute("UPDATE deliveries SET status=?3,reason=?2,revision=revision+1 WHERE message_id IN (SELECT id FROM messages WHERE task_id=?1 AND kind IN ('intake','intake.updated')) AND status IN ('queued','blocked')",params![id,reason,if human {"handled"} else {"cancelled"}])?;
            if task.state == "closed" {
                db.execute("UPDATE deliveries SET status='cancelled',reason='任务已拒绝',revision=revision+1 WHERE message_id IN (SELECT id FROM messages WHERE task_id=?1) AND status IN ('queued','blocked')",[id])?;
            }
            save_task(db, &task)?;
            if !human && matches!(decision, IntakeDecision::Accept) {
                // The active member retains coordination responsibility on its
                // claimed delivery. Acceptance does not produce its terminal.
                return Ok(json!({"task":task,"delivery":null}));
            }
            // A successful human intake leaves durable responsibility to arrange or wait.
            let recipient = if matches!(decision, IntakeDecision::Accept) {
                actor
            } else {
                &task.team_snapshot.acceptor
            }
            .to_string();
            let body = serde_json::to_string(
                &json!({"decision":decision,"reason":reason,"next":"由团队负责人安排工作或说明等待；不会自动执行"}),
            )?;
            let event = format!("intake:{}:{}", task.id, task.revision);
            let delivery = enqueue(
                db,
                &mut task,
                actor,
                cause,
                Message {
                    recipient: &recipient,
                    kind: "result",
                    body: &body,
                    reply_to: None,
                    event: Some(&event),
                    mandatory: true,
                },
            )?;
            Ok(json!({"task":task,"delivery":delivery}))
        }
        Command::MessageSend {
            task_id,
            recipient,
            kind,
            body,
            reply_to,
        } => {
            text(body, "消息正文", 65536)?;
            if kind != "work.note" && kind != "work.question" {
                return Err(Error::Invalid("仅允许 work.note 或 work.question".into()));
            }
            let mut task: Task = load(db, "tasks", task_id)?;
            if task.state == "closed" {
                return Err(Error::Conflict("任务已关闭".into()));
            }
            if !participants(&task).contains(&recipient.as_str()) {
                return Err(Error::Forbidden("接收者必须参与当前任务".into()));
            }
            if let Some(reply) = reply_to {
                let belongs: bool = db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND task_id=?2)",
                    params![reply, task_id],
                    |r| r.get(0),
                )?;
                if !belongs {
                    return Err(Error::Invalid("回复引用必须属于同一任务".into()));
                }
            }
            enqueue(
                db,
                &mut task,
                actor,
                cause,
                Message {
                    recipient,
                    kind,
                    body,
                    reply_to: reply_to.as_deref(),
                    event: None,
                    mandatory: false,
                },
            )
        }
        Command::MailboxRespond {
            id,
            revision: expected,
            reason,
        } => {
            text(reason, "处理原因", 65536)?;
            let (version,status,kind,task_state):(u64,String,String,String)=db.query_row("SELECT d.revision,d.status,m.kind,t.state FROM deliveries d JOIN messages m ON m.id=d.message_id JOIN tasks t ON t.id=m.task_id WHERE d.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            revision(version, *expected)?;
            if task_state == "closed"
                || status != "queued"
                || !matches!(kind.as_str(), "work.note" | "work.question" | "result")
            {
                return Err(Error::Conflict(
                    "该投递须通过正式业务操作处理，或已结束/过期".into(),
                ));
            }
            db.execute(
                "UPDATE deliveries SET status='handled',reason=?2,revision=revision+1 WHERE id=?1",
                params![id, reason],
            )?;
            Ok(json!({"id":id,"revision":version+1,"status":"handled","reason":reason}))
        }
        Command::TaskCancel {
            id,
            revision: expected,
            reason,
        } => {
            text(reason, "取消原因", 65536)?;
            let mut task: Task = load(db, "tasks", id)?;
            revision(task.revision, *expected)?;
            if task.state == "closed" {
                return Err(Error::Conflict("任务已关闭".into()));
            }
            crate::blocker::supersede(db, id, "任务已取消")?;
            task.cancellation_requested = true;
            crate::runs::request_task_stop(db, id, reason)?;
            if crate::runs::active_for_task(db, id)? == 0 {
                task.state = "closed".into();
                task.outcome = Some("cancelled".into());
            }
            task.revision += 1;
            crate::decisions::supersede(db, id, None, task.revision, "任务已取消")?;
            save_task(db, &task)?;
            db.execute("UPDATE deliveries SET status='cancelled',reason=?2,revision=revision+1 WHERE message_id IN (SELECT id FROM messages WHERE task_id=?1) AND status IN ('queued','blocked')",params![id,reason])?;
            Ok(json!(task))
        }
    }
}
