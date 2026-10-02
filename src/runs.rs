//! Durable execution ownership. These control methods are trusted service entry
//! points, never member tools. Observed-stop requires actual resource inspection.
use crate::{
    Error, Result,
    model::*,
    store::{Message, Store, enqueue_system, load, new_id, participants, require, save_task, text},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::collections::BTreeMap;

pub(crate) fn save(db: &Connection, run: &Run) -> Result<()> {
    db.execute("INSERT INTO runs(id,task_id,worker_id,delivery_id,state,data) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET state=excluded.state,data=excluded.data",params![run.id,run.task_id,run.worker_id,run.delivery_id,run.state,serde_json::to_string(run)?])?;
    Ok(())
}
pub(crate) fn active_count(db: &Connection) -> Result<u64> {
    Ok(db.query_row(
        "SELECT count(*) FROM runs WHERE state!='stopped'",
        [],
        |r| r.get(0),
    )?)
}
pub(crate) fn active_for_task(db: &Connection, task_id: &str) -> Result<u64> {
    Ok(db.query_row(
        "SELECT count(*) FROM runs WHERE task_id=?1 AND state!='stopped'",
        [task_id],
        |r| r.get(0),
    )?)
}
fn active(db: &Connection) -> Result<Vec<Run>> {
    let mut stmt = db.prepare("SELECT data FROM runs WHERE state!='stopped'")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.iter()
        .map(|s| serde_json::from_str(s).map_err(Into::into))
        .collect()
}
pub(crate) fn service(db: &Connection, epoch: &str, starting: bool) -> Result<()> {
    let state: Option<(String, bool)> = db
        .query_row(
            "SELECT state,stop_requested FROM runtime WHERE singleton=1 AND epoch=?1",
            [epoch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match state {
        Some((state, stop)) if state == "running" && (!starting || !stop) => Ok(()),
        _ => Err(Error::Conflict("运行服务 epoch 已失效或正在停止".into())),
    }
}
fn required_permission(purpose: &str) -> Permission {
    match purpose {
        "execute" | "rework" => Permission::Execute,
        "verify" => Permission::Verify,
        _ => Permission::Communicate,
    }
}
pub(crate) fn check_run_authority(db: &Connection, run: &Run) -> Result<()> {
    let task: Task = load(db, "tasks", &run.task_id)?;
    if task.state == "closed" || task.cancellation_requested {
        return Err(Error::Conflict("任务已关闭或请求取消".into()));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    require(&current, &run.worker_id, required_permission(&run.purpose))?;
    require(&current, &run.worker_id, Permission::Communicate)?;
    if run.stop_requested {
        return Err(Error::Conflict("本 Run 已请求停止".into()));
    }
    Ok(())
}
pub(crate) fn stopped(db: &Connection, run: &mut Run, reason: &str) -> Result<()> {
    crate::cli_resources::before_stop(db, run)?;
    crate::verification::before_stop(db, run)?;
    run.state = "stopped".into();
    run.stop_requested = true;
    run.stop_reason = Some(reason.into());
    save(db, run)?;
    let mut task: Task = load(db, "tasks", &run.task_id)?;
    let status = if task.cancellation_requested {
        "cancelled"
    } else if crate::disposition::valid_for_stop(db, run, &task)? {
        "handled"
    } else if task.state == "closed" || run.task_revision != task.revision {
        "cancelled"
    } else {
        "blocked"
    };
    // The saved handling result remains queryable even if cancellation or a
    // newer task revision invalidates it. Process exit never supplies one.
    let delivery_reason = if run.task_revision != task.revision && status == "cancelled" {
        "投递的任务依据已过期"
    } else {
        reason
    };
    db.execute(
        "UPDATE deliveries SET status=?2,reason=?3,revision=revision+1 WHERE id=?1 AND run_id=?4",
        params![run.delivery_id, status, delivery_reason, run.id],
    )?;
    if task.cancellation_requested && active_for_task(db, &task.id)? == 0 {
        task.state = "closed".into();
        task.outcome = Some("cancelled".into());
        task.revision += 1;
        save_task(db, &task)?;
    }
    if status == "blocked" {
        notify_failure(db, run, &mut task)?;
    }
    Ok(())
}

fn notify_failure(db: &Connection, run: &Run, task: &mut Task) -> Result<()> {
    if task.state == "closed" || task.cancellation_requested {
        return Ok(());
    }
    if crate::recovery::ensure(
        db,
        task,
        &run.delivery_id,
        &format!("run:{}", run.id),
        run.stop_reason.as_deref().unwrap_or("团队负责人运行受阻"),
    )? {
        return Ok(());
    }
    let stopping: bool = db.query_row(
        "SELECT stop_requested FROM runtime WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    let mut recipients = vec![task.team_snapshot.acceptor.clone()];
    if !stopping
        && task.runs_used < task.contract.max_runs
        && task.team_snapshot.leader != run.worker_id
    {
        recipients.push(task.team_snapshot.leader.clone());
    }
    recipients.sort();
    recipients.dedup();
    for recipient in recipients {
        let event = format!("run-blocked:{}:{recipient}", run.id);
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE event_key=?1)",
            [&event],
            |r| r.get(0),
        )?;
        if exists {
            continue;
        }
        let body = serde_json::json!({"runId":run.id,"deliveryId":run.delivery_id,"reason":run.stop_reason}).to_string();
        enqueue_system(
            db,
            task,
            &run.id,
            Message {
                recipient: &recipient,
                kind: "failure",
                body: &body,
                reply_to: None,
                event: Some(&event),
                mandatory: true,
            },
        )?;
    }
    Ok(())
}
pub(crate) fn recover_on_start(db: &Connection, new_epoch: &str) -> Result<()> {
    for mut run in active(db)? {
        if run.epoch == new_epoch {
            continue;
        }
        if !run.launch_started {
            stopped(db, &mut run, "服务在启动资源前退出；保留原投递待核对")?;
        } else {
            run.state = "unknown".into();
            run.stop_requested = true;
            run.stop_reason = Some("旧服务退出，资源状态尚未核对".into());
            save(db, &run)?;
            db.execute("UPDATE deliveries SET status='uncertain',reason='旧资源尚未核对',revision=revision+1 WHERE id=?1 AND run_id=?2",params![run.delivery_id,run.id])?;
            let mut task: Task = load(db, "tasks", &run.task_id)?;
            notify_failure(db, &run, &mut task)?;
        }
    }
    Ok(())
}
pub(crate) fn request_all_stop(db: &Connection, reason: &str) -> Result<()> {
    for mut run in active(db)? {
        run.stop_requested = true;
        run.stop_reason = Some(reason.into());
        save(db, &run)?;
    }
    Ok(())
}
pub(crate) fn request_task_stop(db: &Connection, task_id: &str, reason: &str) -> Result<()> {
    for mut run in active(db)?.into_iter().filter(|r| r.task_id == task_id) {
        run.stop_requested = true;
        run.stop_reason = Some(reason.into());
        save(db, &run)?;
    }
    Ok(())
}
pub(crate) fn stop_revoked(
    db: &Connection,
    team_id: &str,
    revoke: &BTreeMap<String, Vec<Permission>>,
) -> Result<usize> {
    let mut count = 0;
    for mut run in active(db)? {
        let task: Task = load(db, "tasks", &run.task_id)?;
        if task.team_id == team_id
            && revoke.get(&run.worker_id).is_some_and(|permissions| {
                permissions
                    .iter()
                    .any(|permission| run.permissions.contains(permission))
            })
        {
            run.stop_requested = true;
            run.authority_revoked = true;
            run.stop_reason = Some("成员权限已撤销".into());
            save(db, &run)?;
            count += 1;
        }
    }
    Ok(count)
}

impl Store {
    pub fn run(&self, id: &str) -> Result<Run> {
        load(&self.connection, "runs", id)
    }

    pub fn run_view(&self, id: &str) -> Result<serde_json::Value> {
        let mut value = serde_json::to_value(self.run(id)?)?;
        let record: Option<String> = self
            .connection
            .query_row("SELECT data FROM api_launches WHERE run_id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        value["apiExecution"] = record
            .map(|s| serde_json::from_str::<serde_json::Value>(&s))
            .transpose()?
            .unwrap_or(serde_json::Value::Null);
        value["cliResources"] = serde_json::to_value(self.cli_resources(id)?)?;
        Ok(value)
    }

    /// Service-only: environment/capability preparation precedes this call.
    /// It records acceptance of a run, not successful process startup.
    pub fn runtime_claim(
        &mut self,
        epoch: &str,
        delivery_id: &str,
        configuration_id: &str,
    ) -> Result<Run> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service(&tx, epoch, true)?;
        if active_count(&tx)? > 0 {
            return Err(Error::Conflict("工作区已有活动或未知 Run".into()));
        }
        let row:Option<(String,String,String,String,u64)>=tx.query_row("SELECT d.receiver,d.status,m.task_id,m.kind,m.task_revision FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE d.id=?1",[delivery_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let (receiver, status, task_id, kind, message_revision) =
            row.ok_or_else(|| Error::NotFound("投递不存在".into()))?;
        if status != "queued" {
            return Err(Error::Conflict("投递不是 queued".into()));
        }
        let message_revision =
            crate::retry::effective_revision(&tx, delivery_id, message_revision)?;
        let mut task: Task = load(&tx, "tasks", &task_id)?;
        if task.state == "closed"
            || task.cancellation_requested
            || task.revision != message_revision
        {
            return Err(Error::Conflict("投递的任务依据已过期或任务正在取消".into()));
        }
        let worker = task
            .worker_snapshots
            .get(&receiver)
            .ok_or_else(|| Error::Forbidden("接收者不在任务成员快照中".into()))?;
        if worker.kind != WorkerKind::Agent {
            return Err(Error::Forbidden("人类待办不能创建模型 Run".into()));
        }
        if worker.execution_config.as_deref() != Some(configuration_id) {
            return Err(Error::Conflict("执行配置缺失或与任务快照不同".into()));
        }
        let configuration: crate::connection::ExecutionConfiguration =
            load(&tx, "execution_configs", configuration_id)?;
        if configuration.worker_id != receiver {
            return Err(Error::Forbidden("执行配置不属于该成员".into()));
        }
        let purpose = match kind.as_str() {
            "assignment.execute" => "execute",
            "assignment.rework" => "rework",
            "handoff.verify" => "verify",
            "intake" | "intake.updated" | "work.note" | "work.question" | "result" | "failure"
            | "blocker" | "resolved" | "decision.request" | "decision.result" => "coordinate",
            _ => return Err(Error::Invalid("消息类别没有成员处理契约".into())),
        };
        if purpose != "coordinate" {
            crate::blocker::ensure_clear(&tx, &task)?;
        }
        if purpose != "coordinate" && task.state != "active" {
            return Err(Error::Conflict("未承接任务不能执行或检验".into()));
        }
        let has_role = match purpose {
            "execute" | "rework" => {
                task.team_snapshot.executor.as_deref() == Some(receiver.as_str())
            }
            "verify" => task.team_snapshot.verifier.as_deref() == Some(receiver.as_str()),
            _ => participants(&task).contains(&receiver.as_str()),
        };
        if !has_role {
            return Err(Error::Forbidden(
                "消息接收者不具备任务冻结的对应职责".into(),
            ));
        }
        if purpose == "rework" {
            crate::rework::claimable(&tx, &task, delivery_id)?;
        }
        if purpose == "verify" {
            crate::handoff::claimable(&tx, &task, delivery_id)?;
        }
        let current: Team = load(&tx, "teams", &task.team_id)?;
        for permission in [Permission::Communicate, required_permission(purpose)] {
            require(&current, &receiver, permission.clone())?;
            require(&task.team_snapshot, &receiver, permission)?;
        }
        if matches!(kind.as_str(), "intake" | "intake.updated") {
            if task.team_snapshot.leader != receiver {
                return Err(Error::Forbidden("承接投递只由团队负责人处理".into()));
            }
            require(&current, &receiver, Permission::Arrange)?;
            require(&task.team_snapshot, &receiver, Permission::Arrange)?;
        }
        if task.runs_used >= task.contract.max_runs {
            return Err(Error::Conflict("任务 Run 额度耗尽".into()));
        }
        let frozen = task
            .team_snapshot
            .grants
            .get(&receiver)
            .cloned()
            .unwrap_or_default();
        let permissions = current
            .grants
            .get(&receiver)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| frozen.contains(p))
            .collect();
        let run = Run {
            authority_revoked: false,
            id: new_id(),
            task_id: task.id.clone(),
            worker_id: receiver,
            delivery_id: delivery_id.into(),
            epoch: epoch.into(),
            purpose: purpose.into(),
            state: "prepared".into(),
            task_revision: task.revision,
            configuration_id: configuration_id.into(),
            authorization_revision: current.authorization_revision,
            permissions,
            launch_started: false,
            stop_requested: false,
            stop_reason: None,
            pid: None,
            process_identity: None,
        };
        save(&tx, &run)?;
        tx.execute("UPDATE deliveries SET status='claimed',run_id=?2,claim_epoch=?3,attempts=attempts+1,revision=revision+1 WHERE id=?1",params![delivery_id,run.id,epoch])?;
        task.runs_used += 1;
        if purpose == "rework" {
            task.reworks_used += 1;
        }
        save_task(&tx, &task)?;
        tx.commit()?;
        Ok(run)
    }
    /// Persist this intent before spawning any adapter or execution resource.
    pub fn runtime_begin_launch(&mut self, epoch: &str, id: &str) -> Result<Run> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service(&tx, epoch, true)?;
        let mut run: Run = load(&tx, "runs", id)?;
        if run.epoch != epoch || run.state != "prepared" || run.launch_started {
            return Err(Error::Conflict(
                "Run 不处于本 epoch 的未启动准备状态".into(),
            ));
        }
        check_run_authority(&tx, &run)?;
        run.launch_started = true;
        save(&tx, &run)?;
        tx.commit()?;
        Ok(run)
    }
    /// PID and start identity come from the owned child, never from model output.
    pub fn runtime_child_started(
        &mut self,
        epoch: &str,
        id: &str,
        pid: u32,
        process_identity: &str,
    ) -> Result<Run> {
        text(process_identity, "进程启动标识", 256)?;
        if pid == 0 {
            return Err(Error::Invalid("进程 ID 无效".into()));
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service(&tx, epoch, true)?;
        let mut run: Run = load(&tx, "runs", id)?;
        if run.epoch != epoch || run.state != "prepared" || !run.launch_started {
            return Err(Error::Conflict("Run 启动依据已变化".into()));
        }
        check_run_authority(&tx, &run)?;
        run.pid = Some(pid);
        run.process_identity = Some(process_identity.into());
        run.state = "running".into();
        save(&tx, &run)?;
        tx.commit()?;
        Ok(run)
    }
    /// Current service reports observed uncertainty; no new run can bypass it.
    pub fn runtime_run_unknown(
        &mut self,
        controller_epoch: &str,
        id: &str,
        reason: &str,
    ) -> Result<Run> {
        text(reason, "未知原因", 65536)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service(&tx, controller_epoch, false)?;
        let mut run: Run = load(&tx, "runs", id)?;
        if run.state == "stopped" {
            return Err(Error::Conflict("已确认停止的 Run 不退回未知".into()));
        }
        run.state = "unknown".into();
        run.stop_requested = true;
        run.stop_reason = Some(reason.into());
        save(&tx, &run)?;
        tx.execute("UPDATE deliveries SET status='uncertain',reason=?2,revision=revision+1 WHERE id=?1 AND run_id=?3",params![run.delivery_id,reason,run.id])?;
        let mut task: Task = load(&tx, "tasks", &run.task_id)?;
        notify_failure(&tx, &run, &mut task)?;
        tx.commit()?;
        Ok(run)
    }
    /// Trusted resource coordinator only: call after inspecting all owned child,
    /// container, proxy and in-flight core operations. No CLI/member shortcut.
    pub fn runtime_run_observed_stopped(
        &mut self,
        controller_epoch: &str,
        id: &str,
        reason: &str,
    ) -> Result<Run> {
        text(reason, "停止核对依据", 65536)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        service(&tx, controller_epoch, false)?;
        let mut run: Run = load(&tx, "runs", id)?;
        if run.state != "stopped" {
            stopped(&tx, &mut run, reason)?;
        }
        tx.commit()?;
        Ok(run)
    }
}
