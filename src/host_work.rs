//! Host commands and deploy verification run in the service, off the database
//! thread. A command does not mean "deploy"; verification binds the result.
use crate::{
    Error, Result,
    content::{self, digest},
    deploy, environment,
    model::*,
    store::{self, Message, enqueue_system, load, new_id, require, revision, save_task, text},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const IMPACT: &str =
    "以本机用户身份执行，不做沙箱；可访问该用户能访问的一切，包括 Atelier CLI 与工作区。";

pub(crate) enum Action {
    Done,
    Command(CommandSpec),
    Verify(VerifySpec),
}

pub(crate) struct CommandSpec {
    pub id: String,
    pub task_id: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub log_path: PathBuf,
}

pub(crate) struct VerifySpec {
    pub id: String,
    pub task_id: String,
    pub method: VerifyMethod,
    pub code_root: PathBuf,
    pub export_dir: String,
    pub changes_file: PathBuf,
    pub changes: Vec<DeployChange>,
    pub artifact_id: String,
    pub artifact_digest: String,
    pub service: Option<HostService>,
    pub log_path: PathBuf,
}

pub(crate) struct StartedCommand {
    pub(crate) spec: CommandSpec,
    pub(crate) child: Child,
    pub(crate) pgid: u32,
}

pub(crate) struct Outcome {
    kind: &'static str,
    id: String,
    task_id: String,
    state: String,
    exit_code: Option<i32>,
    output_tail: Option<String>,
    mismatches: Vec<String>,
    health: Option<String>,
    pgid: Option<u32>,
}

pub(crate) struct SubmittedCommand {
    pub argv: Vec<String>,
    pub cwd: String,
    pub timeout_seconds: u32,
    pub reason: String,
}

pub(crate) fn submit(
    db: &Connection,
    workspace: &Path,
    run: &Run,
    task: &mut Task,
    command: SubmittedCommand,
) -> Result<Value> {
    let SubmittedCommand {
        argv,
        cwd,
        timeout_seconds,
        reason,
    } = command;
    let cwd = cwd.as_str();
    let reason = reason.as_str();
    ensure_deployer(db, run, task)?;
    text(reason, "命令理由", 65536)?;
    environment::validate_argv(&argv)?;
    if !(1..=3600).contains(&timeout_seconds) {
        return Err(Error::Invalid("命令超时须在 1 到 3600 秒之间".into()));
    }
    let snapshot = task
        .environment_snapshot
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有点名部署目标环境".into()))?;
    let record = deploy::deploy_mut(task)?;
    if record.commands.iter().any(|command| {
        matches!(
            command.state.as_str(),
            "awaiting_approval" | "queued" | "running"
        )
    }) || record
        .verifications
        .iter()
        .any(|item| matches!(item.state.as_str(), "queued" | "running"))
    {
        return Err(Error::Conflict("已有一条命令或核对在进行".into()));
    }
    environment::require_current(db, &snapshot)?;
    let absolute = resolve_cwd(Path::new(&snapshot.code_root), cwd)?;
    let id = new_id();
    let approval = snapshot.approval.clone();
    let mut command = HostCommand {
        id: id.clone(),
        run_id: run.id.clone(),
        argv: argv.clone(),
        cwd: cwd.into(),
        timeout_seconds,
        reason: reason.into(),
        state: "queued".into(),
        decision_id: None,
        epoch: None,
        pgid: None,
        exit_code: None,
        output_tail: None,
        log_path: Some(
            log_path(workspace, "host-commands", &id)
                .to_string_lossy()
                .into_owned(),
        ),
        started_at: None,
        ended_at: None,
    };
    if approval == CommandApproval::Ask {
        command.state = "awaiting_approval".into();
        let decision_id = new_id();
        command.decision_id = Some(decision_id.clone());
        let question = format!(
            "环境 {}\n工作目录 {}\n参数 {}\n理由 {}",
            snapshot.name,
            absolute.display(),
            serde_json::to_string(&argv)?,
            reason
        );
        record.commands.push(command);
        let delivery = enqueue_decision(db, task, &run.worker_id, &decision_id, &question)?;
        save_decision(db, task, &run.worker_id, &decision_id, &question, &delivery)?;
    } else {
        record.commands.push(command);
        save_task(db, task)?;
    }
    let _ = absolute;
    Ok(
        json!({"commandId": id, "state": if approval == CommandApproval::Ask {"awaiting_approval"} else {"queued"}}),
    )
}

pub(crate) fn request_verification(
    db: &Connection,
    actor: &str,
    task_id: &str,
    expected: u64,
    run_id: Option<&str>,
) -> Result<Value> {
    let mut task: Task = load(db, "tasks", task_id)?;
    revision(task.revision, expected)?;
    runtime_available(db)?;
    if task.state != "active" || task.cancellation_requested {
        return Err(Error::Conflict("任务已关闭或正在取消".into()));
    }
    let deployer = task
        .team_snapshot
        .deployer
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有冻结部署职责".into()))?;
    if actor != deployer {
        return Err(Error::Forbidden("只能由冻结的部署成员发起核对".into()));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    require(&current, actor, Permission::Deploy)?;
    require(&task.team_snapshot, actor, Permission::Deploy)?;
    let worker = task
        .worker_snapshots
        .get(actor)
        .ok_or_else(|| Error::Conflict("部署成员快照缺失".into()))?;
    match (&worker.kind, run_id) {
        (WorkerKind::Human, _) => {
            if crate::runs::active_for_task(db, &task.id)? > 0 {
                return Err(Error::Conflict("仍有活动 Run，不能发起核对".into()));
            }
        }
        (WorkerKind::Agent, Some(run_id)) => {
            let run: Run = load(db, "runs", run_id)?;
            if run.worker_id != actor || run.purpose != "deploy" || run.state != "running" {
                return Err(Error::Forbidden(
                    "数字员工须在自己的部署运行中发起核对".into(),
                ));
            }
        }
        (WorkerKind::Agent, None) => {
            return Err(Error::Forbidden(
                "数字员工须在自己的部署运行中发起核对".into(),
            ));
        }
    }
    let snapshot = task
        .environment_snapshot
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有点名部署目标环境".into()))?;
    environment::require_current(db, &snapshot)?;
    let record = deploy::deploy_mut(&mut task)?;
    if record.commands.iter().any(|command| {
        matches!(
            command.state.as_str(),
            "awaiting_approval" | "queued" | "running"
        )
    }) || record
        .verifications
        .iter()
        .any(|item| matches!(item.state.as_str(), "queued" | "running"))
    {
        return Err(Error::Conflict("已有一条命令或核对在进行".into()));
    }
    let id = new_id();
    let method = match &snapshot.verification {
        VerifyMethod::Files => "files",
        VerifyMethod::Command { .. } => "command",
    };
    record.verifications.push(DeployVerification {
        id: id.clone(),
        requested_by: actor.into(),
        state: "queued".into(),
        epoch: None,
        method: method.into(),
        mismatches: Vec::new(),
        exit_code: None,
        output_tail: None,
        log_path: None,
        health: None,
        ended_at: None,
    });
    save_task(db, &task)?;
    Ok(json!({"verificationId": id}))
}

pub(crate) fn respond(
    db: &Connection,
    actor: &str,
    id: &str,
    expected: u64,
    answer: &str,
) -> Result<Value> {
    let mut decision: DecisionRequest = load(db, "decisions", id)?;
    revision(decision.revision, expected)?;
    if decision.kind != "host_command" || decision.state != "open" || decision.handler != actor {
        return Err(Error::Forbidden("只能由本人回应尚未决定的本机命令".into()));
    }
    if answer != "执行" && answer != "拒绝" {
        return Err(Error::Invalid("回应不属于允许的选项".into()));
    }
    let mut task: Task = load(db, "tasks", &decision.task_id)?;
    if task.revision != decision.effective_revision {
        return Err(Error::Conflict(format!(
            "决定依据已过期，当前任务版本为 {}",
            task.revision
        )));
    }
    let record = deploy::deploy_mut(&mut task)?;
    let command = record
        .commands
        .iter_mut()
        .find(|command| command.decision_id.as_deref() == Some(id))
        .ok_or_else(|| Error::Conflict("待确认的命令不存在".into()))?;
    if command.state != "awaiting_approval" {
        return Err(Error::Conflict("命令已不在等待确认".into()));
    }
    decision.answer = Some(answer.into());
    decision.state = "responded".into();
    decision.revision += 1;
    if answer == "执行" {
        command.state = "queued".into();
    } else {
        command.state = "rejected".into();
        command.ended_at = Some(now());
    }
    let command_id = command.id.clone();
    let rejected = answer == "拒绝";
    crate::decisions::save(db, &decision)?;
    if rejected {
        deliver_command(db, &mut task, &command_id, "rejected", None)?;
    } else {
        save_task(db, &task)?;
    }
    if load::<Worker>(db, "workers", actor)?.kind == WorkerKind::Human {
        db.execute(
            "UPDATE deliveries SET status='handled',reason='本机命令已确认',revision=revision+1 WHERE id=?1 AND run_id IS NULL",
            [&decision.request_delivery],
        )?;
    }
    Ok(
        json!({"decision": decision, "commandId": command_id, "state": if rejected {"rejected"} else {"queued"}}),
    )
}

pub(crate) fn cancel_waiting(db: &Connection, task_id: &str) -> Result<()> {
    let Ok(mut task) = load::<Task>(db, "tasks", task_id) else {
        return Ok(());
    };
    let Some(record) = task.deploy.as_mut() else {
        return Ok(());
    };
    let mut changed = false;
    for command in &mut record.commands {
        if command.state == "awaiting_approval" || command.state == "queued" {
            command.state = "cancelled".into();
            command.ended_at = Some(now());
            changed = true;
        }
    }
    if changed {
        save_task(db, &task)?;
    }
    Ok(())
}

pub(crate) fn recover(db: &mut Connection, epoch: &str) -> Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let tasks = open_tasks(&tx)?;
    for mut task in tasks {
        let Some(record) = task.deploy.as_mut() else {
            continue;
        };
        let mut notify = Vec::new();
        for command in &mut record.commands {
            match command.state.as_str() {
                "running" if command.epoch.as_deref() != Some(epoch) => {
                    command.state = "interrupted".into();
                    command.ended_at = Some(now());
                    command.output_tail = Some("中断，结果未知".into());
                    notify.push(command.id.clone());
                }
                "queued" if command.epoch.as_deref().is_some_and(|owner| owner != epoch) => {
                    command.epoch = None;
                }
                _ => {}
            }
        }
        for verification in &mut record.verifications {
            if verification.state == "running" && verification.epoch.as_deref() != Some(epoch) {
                verification.state = "interrupted".into();
                verification.ended_at = Some(now());
                verification.output_tail = Some("中断，结果未知".into());
            }
            if verification.state == "queued"
                && verification
                    .epoch
                    .as_deref()
                    .is_some_and(|owner| owner != epoch)
            {
                verification.epoch = None;
            }
        }
        save_task(&tx, &task)?;
        for id in notify {
            deliver_command(&tx, &mut task, &id, "interrupted", None)?;
        }
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn claim(db: &mut Connection, epoch: &str, workspace: &Path) -> Result<Option<Action>> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if busy(&tx)? {
        return Ok(None);
    }
    let tasks = open_tasks(&tx)?;
    for mut task in tasks {
        if crate::runs::active_for_task(&tx, &task.id)? > 0 {
            continue;
        }
        let Some(snapshot) = task.environment_snapshot.clone() else {
            continue;
        };
        let export = task
            .deploy
            .as_ref()
            .and_then(|record| record.export.clone());
        let Some(export) = export else {
            continue;
        };
        if let Some(command) = task
            .deploy
            .as_ref()
            .and_then(|record| {
                record
                    .commands
                    .iter()
                    .find(|command| command.state == "queued")
            })
            .cloned()
        {
            if !environment::unchanged(&tx, &snapshot)? {
                settle_command(
                    &tx,
                    &mut task,
                    &command.id,
                    "cancelled",
                    None,
                    Some("部署目标登记已变化"),
                )?;
                tx.commit()?;
                return Ok(Some(Action::Done));
            }
            let command_id = command.id.clone();
            mark_command(&tx, &mut task, &command_id, epoch)?;
            tx.commit()?;
            let cwd = resolve_cwd(Path::new(&snapshot.code_root), &command.cwd)?;
            return Ok(Some(Action::Command(CommandSpec {
                log_path: log_path(workspace, "host-commands", &command_id),
                id: command_id,
                task_id: task.id,
                argv: command.argv,
                cwd,
                timeout: Duration::from_secs(u64::from(command.timeout_seconds)),
            })));
        }
        if let Some(verification) = task
            .deploy
            .as_ref()
            .and_then(|record| {
                record
                    .verifications
                    .iter()
                    .find(|item| item.state == "queued")
            })
            .cloned()
        {
            if !environment::unchanged(&tx, &snapshot)? {
                settle_verification(
                    &tx,
                    &mut task,
                    &verification.id,
                    SettledVerification {
                        state: "failed".into(),
                        mismatches: vec!["登记已变化".into()],
                        exit_code: None,
                        output_tail: None,
                        health: None,
                    },
                )?;
                tx.commit()?;
                return Ok(Some(Action::Done));
            }
            let artifact: crate::artifact::Artifact = load(&tx, "artifacts", &export.artifact_id)?;
            let verification_id = verification.id.clone();
            mark_verification(&tx, &mut task, &verification_id, epoch)?;
            tx.commit()?;
            return Ok(Some(Action::Verify(VerifySpec {
                id: verification_id,
                task_id: task.id,
                method: snapshot.verification,
                code_root: PathBuf::from(snapshot.code_root),
                changes_file: deploy::changes_file(&export.dir),
                export_dir: export.dir,
                changes: export.changes,
                artifact_id: export.artifact_id,
                artifact_digest: artifact.content_digest,
                service: snapshot.service,
                log_path: log_path(workspace, "deploy-verifications", &verification.id),
            })));
        }
    }
    Ok(None)
}

impl Outcome {
    pub(crate) fn interrupted(id: &str, task_id: &str, message: &str) -> Self {
        Self {
            kind: "command",
            id: id.into(),
            task_id: task_id.into(),
            state: "interrupted".into(),
            exit_code: None,
            output_tail: Some(message.into()),
            mismatches: Vec::new(),
            health: None,
            pgid: None,
        }
    }
}

pub(crate) fn stop_started(started: &mut StartedCommand) {
    signal_group(started.pgid, "-TERM");
    let _ = started.child.kill();
}

pub(crate) fn spawn_command(spec: CommandSpec) -> Result<StartedCommand> {
    let child = spawn(&spec.argv, &spec.cwd, &spec.log_path, &[])?;
    let pgid = child.id();
    Ok(StartedCommand { spec, child, pgid })
}

pub(crate) fn record_pgid(
    db: &mut Connection,
    epoch: &str,
    id: &str,
    task_id: &str,
    pgid: u32,
) -> Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut task: Task = load(&tx, "tasks", task_id)?;
    let command = command_mut(&mut task, id)?;
    if command.epoch.as_deref() != Some(epoch) || command.state != "running" {
        return Err(Error::Conflict("命令已不属于当前运行服务".into()));
    }
    command.pgid = Some(pgid);
    save_task(&tx, &task)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn wait_command(mut started: StartedCommand) -> Result<Outcome> {
    let deadline = Instant::now() + started.spec.timeout;
    let status = loop {
        if let Some(status) = started.child.try_wait()? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            signal_group(started.pgid, "-TERM");
            thread::sleep(Duration::from_secs(5));
            signal_group(started.pgid, "-KILL");
            let _ = started.child.wait();
            return Ok(command_outcome(&started, "timed_out", None));
        }
        thread::sleep(Duration::from_millis(50));
    };
    signal_group(started.pgid, "-TERM");
    thread::sleep(Duration::from_millis(200));
    signal_group(started.pgid, "-KILL");
    let code = status.and_then(|status| status.code());
    Ok(command_outcome(&started, "exited", code))
}

pub(crate) fn execute_verify(spec: VerifySpec) -> Result<Outcome> {
    let (mismatches, exit_code, output_tail) = match &spec.method {
        VerifyMethod::Files => (compare_files(&spec.code_root, &spec.changes), None, None),
        VerifyMethod::Command {
            argv,
            timeout_seconds,
        } => {
            let mut child = match spawn(
                argv,
                &spec.code_root,
                &spec.log_path,
                &[
                    (
                        "ATELIER_CODE_ROOT",
                        spec.code_root.to_string_lossy().as_ref(),
                    ),
                    ("ATELIER_EXPORT_DIR", spec.export_dir.as_str()),
                    (
                        "ATELIER_CHANGES_FILE",
                        spec.changes_file.to_string_lossy().as_ref(),
                    ),
                    ("ATELIER_ARTIFACT_ID", spec.artifact_id.as_str()),
                    ("ATELIER_ARTIFACT_DIGEST", spec.artifact_digest.as_str()),
                    ("ATELIER_TASK_ID", spec.task_id.as_str()),
                ],
            ) {
                Ok(child) => child,
                Err(error) => {
                    return Ok(verification_outcome(
                        &spec,
                        None,
                        None,
                        vec![format!("核对命令无法启动：{error}")],
                        None,
                    ));
                }
            };
            let pgid = child.id();
            let deadline = Instant::now() + Duration::from_secs(u64::from(*timeout_seconds));
            let status = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => {}
                    Err(error) => {
                        signal_group(pgid, "-KILL");
                        let _ = child.wait();
                        return Ok(verification_outcome(
                            &spec,
                            None,
                            tail(&spec.log_path),
                            vec![format!("核对命令无法等待：{error}")],
                            spec.service.as_ref().map(probe_health),
                        ));
                    }
                }
                if Instant::now() >= deadline {
                    signal_group(pgid, "-TERM");
                    thread::sleep(Duration::from_secs(5));
                    signal_group(pgid, "-KILL");
                    let _ = child.wait();
                    return Ok(Outcome {
                        kind: "verification",
                        id: spec.id,
                        task_id: spec.task_id,
                        state: "failed".into(),
                        exit_code: None,
                        output_tail: tail(&spec.log_path),
                        mismatches: vec!["核对命令超时".into()],
                        health: spec.service.as_ref().map(probe_health),
                        pgid: Some(pgid),
                    });
                }
                thread::sleep(Duration::from_millis(50));
            };
            signal_group(pgid, "-TERM");
            thread::sleep(Duration::from_millis(200));
            signal_group(pgid, "-KILL");
            let code = status.and_then(|status| status.code());
            let mismatches = if code == Some(0) {
                Vec::new()
            } else {
                vec![format!(
                    "核对命令退出码 {}",
                    code.map(|code| code.to_string())
                        .unwrap_or_else(|| "unknown".into())
                )]
            };
            (mismatches, code, tail(&spec.log_path))
        }
    };
    let health = spec.service.as_ref().map(probe_health);
    Ok(verification_outcome(
        &spec,
        exit_code,
        output_tail,
        mismatches,
        health,
    ))
}

fn verification_outcome(
    spec: &VerifySpec,
    exit_code: Option<i32>,
    output_tail: Option<String>,
    mismatches: Vec<String>,
    health: Option<String>,
) -> Outcome {
    let passed = mismatches.is_empty() && health.as_deref().is_none_or(|value| value == "200");
    Outcome {
        kind: "verification",
        id: spec.id.clone(),
        task_id: spec.task_id.clone(),
        state: if passed { "passed" } else { "failed" }.into(),
        exit_code,
        output_tail,
        mismatches,
        health,
        pgid: None,
    }
}

pub(crate) fn finish(db: &mut Connection, epoch: &str, outcome: Outcome) -> Result<()> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut task: Task = load(&tx, "tasks", &outcome.task_id)?;
    if outcome.kind == "command" {
        let command = command_mut(&mut task, &outcome.id)?;
        if command.epoch.as_deref() != Some(epoch) || command.state != "running" {
            return Err(Error::Conflict("命令已不属于当前运行服务".into()));
        }
        command.state = outcome.state.clone();
        command.exit_code = outcome.exit_code;
        command.output_tail = outcome.output_tail.clone();
        command.ended_at = Some(now());
        if command.pgid.is_none() {
            command.pgid = outcome.pgid;
        }
        let id = outcome.id.clone();
        let state = outcome.state.clone();
        let code = outcome.exit_code;
        if task.state == "active" {
            deliver_command(&tx, &mut task, &id, &state, code)?;
        } else {
            save_task(&tx, &task)?;
        }
    } else {
        let verification = task
            .deploy
            .as_mut()
            .and_then(|record| {
                record
                    .verifications
                    .iter_mut()
                    .find(|item| item.id == outcome.id)
            })
            .ok_or_else(|| Error::Conflict("核对不存在".into()))?;
        if verification.epoch.as_deref() != Some(epoch) || verification.state != "running" {
            return Err(Error::Conflict("核对已不属于当前运行服务".into()));
        }
        verification.exit_code = outcome.exit_code;
        verification.output_tail = outcome.output_tail.clone();
        verification.mismatches = outcome.mismatches.clone();
        verification.health = outcome.health.clone();
        verification.ended_at = Some(now());
        if outcome.state == "passed" && task.state == "active" {
            verification.state = "passed".into();
            deploy::close_succeeded(&tx, &mut task, "核对通过")?;
        } else {
            verification.state = "failed".into();
            let id = outcome.id.clone();
            deliver_verification(&tx, &mut task, &id)?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn command_outcome(started: &StartedCommand, state: &str, exit_code: Option<i32>) -> Outcome {
    Outcome {
        kind: "command",
        id: started.spec.id.clone(),
        task_id: started.spec.task_id.clone(),
        state: state.into(),
        exit_code,
        output_tail: tail(&started.spec.log_path),
        mismatches: Vec::new(),
        health: None,
        pgid: Some(started.pgid),
    }
}

fn ensure_deployer(db: &Connection, run: &Run, task: &Task) -> Result<()> {
    if run.purpose != "deploy"
        || task.team_snapshot.deployer.as_deref() != Some(run.worker_id.as_str())
        || !run.permissions.contains(&Permission::Deploy)
    {
        return Err(Error::Forbidden(
            "只有冻结部署成员的部署运行可以执行本机命令".into(),
        ));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    require(&current, &run.worker_id, Permission::Deploy)?;
    require(&task.team_snapshot, &run.worker_id, Permission::Deploy)?;
    Ok(())
}

fn resolve_cwd(code_root: &Path, cwd: &str) -> Result<PathBuf> {
    let code_root = fs::canonicalize(code_root)?;
    if cwd == "." {
        return Ok(code_root);
    }
    content::relative_path(cwd)?;
    let joined = code_root.join(cwd);
    let canonical = fs::canonicalize(&joined)
        .map_err(|_| Error::Invalid("工作目录不存在或不在代码目录内".into()))?;
    if !canonical.starts_with(&code_root) {
        return Err(Error::Invalid("工作目录越出代码目录".into()));
    }
    Ok(canonical)
}

fn enqueue_decision(
    db: &Connection,
    task: &mut Task,
    requester: &str,
    decision_id: &str,
    question: &str,
) -> Result<String> {
    let self_id: String = db.query_row("SELECT self_id FROM workspace", [], |row| row.get(0))?;
    let body = json!({"decisionId": decision_id, "question": question, "impact": IMPACT, "options": ["执行", "拒绝"]}).to_string();
    let delivery = store::enqueue(
        db,
        task,
        requester,
        decision_id,
        Message {
            recipient: &self_id,
            kind: "decision.request",
            body: &body,
            reply_to: None,
            event: Some(&format!("decision:{decision_id}")),
            mandatory: false,
        },
    )?;
    Ok(delivery["deliveryId"]
        .as_str()
        .ok_or_else(|| Error::Invalid("决定投递缺失".into()))?
        .to_string())
}

fn save_decision(
    db: &Connection,
    task: &Task,
    requester: &str,
    decision_id: &str,
    question: &str,
    delivery: &str,
) -> Result<()> {
    let self_id: String = db.query_row("SELECT self_id FROM workspace", [], |row| row.get(0))?;
    let decision = DecisionRequest {
        recovery: None,
        acceptance: None,
        id: decision_id.into(),
        task_id: task.id.clone(),
        task_revision: task.revision,
        effective_revision: task.revision,
        requester: requester.into(),
        handler: self_id,
        kind: "host_command".into(),
        question: question.into(),
        impact: IMPACT.into(),
        options: vec!["执行".into(), "拒绝".into()],
        revision: 1,
        state: "open".into(),
        answer: None,
        blocked_reason: None,
        reason: None,
        operation: None,
        request_delivery: delivery.into(),
        response_delivery: None,
    };
    crate::decisions::save(db, &decision)
}

fn runtime_available(db: &Connection) -> Result<()> {
    let row: Option<(String, bool)> = db
        .query_row(
            "SELECT state,stop_requested FROM runtime WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match row {
        Some((state, false)) if state == "running" => Ok(()),
        _ => Err(Error::Unavailable("运行服务未在运行，不能发起核对".into())),
    }
}

fn open_tasks(db: &Connection) -> Result<Vec<Task>> {
    let mut statement = db.prepare(
        "SELECT data FROM tasks WHERE state='active' AND json_extract(data,'$.deploy.state')='open' ORDER BY rowid",
    )?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.iter()
        .map(|row| serde_json::from_str(row).map_err(Error::from))
        .collect()
}

fn busy(db: &Connection) -> Result<bool> {
    let tasks = open_tasks(db)?;
    Ok(tasks.iter().any(|task| {
        task.deploy.as_ref().is_some_and(|record| {
            record
                .commands
                .iter()
                .any(|command| command.state == "running")
                || record
                    .verifications
                    .iter()
                    .any(|item| item.state == "running")
        })
    }))
}

fn command_mut<'a>(task: &'a mut Task, id: &str) -> Result<&'a mut HostCommand> {
    task.deploy
        .as_mut()
        .and_then(|record| record.commands.iter_mut().find(|command| command.id == id))
        .ok_or_else(|| Error::Conflict("命令不存在".into()))
}

fn mark_command(db: &Connection, task: &mut Task, id: &str, epoch: &str) -> Result<()> {
    let command = command_mut(task, id)?;
    command.state = "running".into();
    command.epoch = Some(epoch.into());
    command.started_at = Some(now());
    save_task(db, task)
}

fn mark_verification(db: &Connection, task: &mut Task, id: &str, epoch: &str) -> Result<()> {
    let verification = task
        .deploy
        .as_mut()
        .and_then(|record| record.verifications.iter_mut().find(|item| item.id == id))
        .ok_or_else(|| Error::Conflict("核对不存在".into()))?;
    verification.state = "running".into();
    verification.epoch = Some(epoch.into());
    save_task(db, task)
}

fn settle_command(
    db: &Connection,
    task: &mut Task,
    id: &str,
    state: &str,
    code: Option<i32>,
    tail_text: Option<&str>,
) -> Result<()> {
    let command = command_mut(task, id)?;
    command.state = state.into();
    command.exit_code = code;
    command.output_tail = tail_text.map(str::to_string);
    command.ended_at = Some(now());
    deliver_command(db, task, id, state, code)
}

struct SettledVerification {
    state: String,
    mismatches: Vec<String>,
    exit_code: Option<i32>,
    output_tail: Option<String>,
    health: Option<String>,
}

fn settle_verification(
    db: &Connection,
    task: &mut Task,
    id: &str,
    settled: SettledVerification,
) -> Result<()> {
    let SettledVerification {
        state,
        mismatches,
        exit_code,
        output_tail,
        health,
    } = settled;
    let state = state.as_str();
    let verification = task
        .deploy
        .as_mut()
        .and_then(|record| record.verifications.iter_mut().find(|item| item.id == id))
        .ok_or_else(|| Error::Conflict("核对不存在".into()))?;
    verification.state = state.into();
    verification.mismatches = mismatches;
    verification.exit_code = exit_code;
    verification.output_tail = output_tail;
    verification.health = health;
    verification.ended_at = Some(now());
    deliver_verification(db, task, id)
}

fn deliver_command(
    db: &Connection,
    task: &mut Task,
    id: &str,
    state: &str,
    exit_code: Option<i32>,
) -> Result<()> {
    let deployer = task
        .team_snapshot
        .deployer
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有冻结部署职责".into()))?;
    let body = json!({"commandId": id, "state": state, "exitCode": exit_code}).to_string();
    enqueue_system(
        db,
        task,
        &format!("host-command:{id}:{state}"),
        Message {
            recipient: &deployer,
            kind: "host_command.result",
            body: &body,
            reply_to: None,
            event: Some(&format!("host-command:{id}:{state}")),
            mandatory: false,
        },
    )?;
    Ok(())
}

fn deliver_verification(db: &Connection, task: &mut Task, id: &str) -> Result<()> {
    let deployer = task
        .team_snapshot
        .deployer
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有冻结部署职责".into()))?;
    let verification = task
        .deploy
        .as_ref()
        .and_then(|record| record.verifications.iter().find(|item| item.id == id))
        .ok_or_else(|| Error::Conflict("核对不存在".into()))?;
    let body = json!({
        "verificationId": id,
        "state": verification.state,
        "mismatches": verification.mismatches,
        "exitCode": verification.exit_code,
        "health": verification.health
    })
    .to_string();
    enqueue_system(
        db,
        task,
        &format!("deploy-verification:{id}"),
        Message {
            recipient: &deployer,
            kind: "deploy.verification",
            body: &body,
            reply_to: None,
            event: Some(&format!("deploy-verification:{id}:{}", verification.state)),
            mandatory: false,
        },
    )?;
    Ok(())
}

fn spawn(argv: &[String], cwd: &Path, log_path: &Path, env: &[(&str, &str)]) -> Result<Child> {
    #[cfg(not(unix))]
    {
        let _ = (argv, cwd, log_path, env);
        return Err(Error::Unavailable("本机命令只支持 macOS 与 Linux".into()));
    }
    #[cfg(unix)]
    {
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;
        let errors = log.try_clone()?;
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(errors));
        for (key, value) in env {
            command.env(key, value);
        }
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        command.spawn().map_err(Error::Io)
    }
}

fn signal_group(pgid: u32, signal: &str) {
    if pgid <= 1 {
        return;
    }
    let _ = Command::new("/bin/kill")
        .args([signal, &format!("-{pgid}")])
        .status();
}

fn compare_files(root: &Path, changes: &[DeployChange]) -> Vec<String> {
    let mut mismatches = Vec::new();
    for change in changes {
        match change.action.as_str() {
            "delete" => match path_exists_without_symlink(root, &change.path) {
                Ok(true) => mismatches.push(format!("{}: 应删除但仍存在", change.path)),
                Ok(false) => {}
                Err(reason) => mismatches.push(format!("{}: {reason}", change.path)),
            },
            "write" => match file_facts(root, &change.path) {
                Ok((hash, executable)) => {
                    if Some(hash.as_str()) != change.sha256.as_deref()
                        || Some(executable) != change.executable
                    {
                        mismatches.push(format!("{}: 内容或可执行位与产出不一致", change.path));
                    }
                }
                Err(reason) => mismatches.push(format!("{}: {reason}", change.path)),
            },
            _ => mismatches.push(format!("{}: 未知改动", change.path)),
        }
    }
    mismatches
}

fn path_exists_without_symlink(root: &Path, relative: &str) -> std::result::Result<bool, String> {
    let mut current = root.to_path_buf();
    for component in relative.split('/') {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("路径含符号链接".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(true)
}

fn file_facts(root: &Path, relative: &str) -> std::result::Result<(String, bool), String> {
    let mut current = root.to_path_buf();
    for component in relative.split('/') {
        current.push(component);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "生产文件不存在".into()
            } else {
                error.to_string()
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err("路径含符号链接".into());
        }
    }
    let metadata = fs::metadata(&current).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("不是普通文件".into());
    }
    let mut file = fs::File::open(&current).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    let executable = {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let executable = false;
    Ok((digest(&bytes), executable))
}

fn probe_health(service: &HostService) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    let address = SocketAddr::from(([127, 0, 0, 1], service.port));
    loop {
        match one_probe(&address, &service.health_path) {
            Ok(()) => return "200".into(),
            Err(message) if Instant::now() >= deadline => return message,
            Err(_) => thread::sleep(Duration::from_millis(500)),
        }
    }
}

fn one_probe(address: &SocketAddr, path: &str) -> std::result::Result<(), String> {
    let mut stream = TcpStream::connect_timeout(address, Duration::from_millis(500))
        .map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| error.to_string())?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut buffer = [0_u8; 128];
    let read = stream
        .read(&mut buffer)
        .map_err(|error| error.to_string())?;
    let line = String::from_utf8_lossy(&buffer[..read]);
    let status = line.split_whitespace().nth(1).unwrap_or("");
    if status == "200" {
        Ok(())
    } else {
        Err(format!("健康检查未返回 200：{line}"))
    }
}

fn tail(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let start = bytes.len().saturating_sub(16 * 1024);
    Some(String::from_utf8_lossy(&bytes[start..]).into_owned())
}

fn log_path(workspace: &Path, directory: &str, id: &str) -> PathBuf {
    workspace.join(directory).join(format!("{id}.log"))
}

fn now() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}
