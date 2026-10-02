//! Explicit retry preserves the delivery and operation ledger; it never resets a Run.
use crate::{
    Error, Result,
    model::*,
    store::{load, require, revision, text},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

pub(crate) fn task_for(db: &Connection, id: &str) -> Result<Task> {
    let task: Option<String> = db
        .query_row(
            "SELECT m.task_id FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE d.id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    load(
        db,
        "tasks",
        &task.ok_or_else(|| Error::NotFound("投递不存在".into()))?,
    )
}
pub(crate) fn authorize(db: &Connection, actor: &str, id: &str) -> Result<Task> {
    let task = task_for(db, id)?;
    let permission = if actor == task.team_snapshot.acceptor
        && load::<Worker>(db, "workers", actor)?.kind == WorkerKind::Human
    {
        Permission::Manage
    } else if actor == task.team_snapshot.leader {
        Permission::Arrange
    } else {
        return Err(Error::Forbidden(
            "仅本人或获准团队负责人可重评估投递".into(),
        ));
    };
    let team: Team = load(db, "teams", &task.team_id)?;
    for p in [permission, Permission::Communicate] {
        require(&team, actor, p.clone())?;
        require(&task.team_snapshot, actor, p)?;
    }
    Ok(task)
}
/// A member may already have advanced the task while handling this immutable message.
/// Only the latest stopped Run on this exact delivery can supply a continuation basis.
pub(crate) fn effective_revision(db: &Connection, id: &str, original: u64) -> Result<u64> {
    let data: Option<String> = db
        .query_row(
            "SELECT r.data FROM deliveries d JOIN runs r ON r.id=d.run_id WHERE d.id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(data) = data {
        let run: Run = serde_json::from_str(&data)?;
        if run.delivery_id == id
            && run.state == "stopped"
            && matches!(run.purpose.as_str(), "coordinate" | "verify")
        {
            return Ok(run.task_revision);
        }
    }
    Ok(original)
}
pub(crate) fn apply(
    db: &Connection,
    actor: &str,
    cause: &str,
    id: &str,
    expected: u64,
    reason: &str,
) -> Result<Value> {
    let task = authorize(db, actor, id)?;
    text(reason, "重评估原因", 65536)?;
    let (status,rev,prior,receiver,message,kind,original):(String,u64,Option<String>,String,String,String,u64)=db.query_row("SELECT d.status,d.revision,d.run_id,d.receiver,m.id,m.kind,m.task_revision FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE d.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    revision(rev, expected)?;
    let response = |state: &str, new_revision: u64| json!({"operation":"mailbox_retry","delivery":{"deliveryId":id,"messageId":message,"status":state,"revision":new_revision},"previousRunId":prior,"taskId":task.id,"reason":reason,"next":if state=="handled" {"已有处理结果，不启动新运行"} else {"已排队供服务重新检查运行条件，不代表环境可用或工作完成"}});
    if status == "handled" {
        crate::recovery::resolve_retried(db, actor, id, cause)?;
        return Ok(response("handled", rev));
    }
    if !matches!(status.as_str(), "blocked" | "uncertain") {
        return Err(Error::Conflict(
            "只能重评估 blocked/uncertain 投递；已处理仅查询结果".into(),
        ));
    }
    if task.state == "closed"
        || task.cancellation_requested
        || effective_revision(db, id, original)? != task.revision
    {
        return Err(Error::Conflict(
            "任务已取消、关闭或原投递的有效依据已过期".into(),
        ));
    }
    if let Some(previous) = &prior {
        let run: Run = load(db, "runs", previous)?;
        if run.state != "stopped" {
            return Err(Error::Conflict(
                "先核对原 Run 全部资源停止，不能凭重试解除未知状态".into(),
            ));
        }
        if crate::disposition::valid_for_stop(db, &run, &task)? {
            db.execute(
                "UPDATE deliveries SET status='handled',reason=?2,revision=revision+1 WHERE id=?1",
                params![id, reason],
            )?;
            crate::recovery::resolve_retried(db, actor, id, cause)?;
            return Ok(response("handled", rev + 1));
        }
        if matches!(run.purpose.as_str(), "execute" | "rework") {
            return Err(Error::Conflict("已受理代码执行不能原样重试；先固定 partial，再由负责人引用已核对失败或阻塞安排返工".into()));
        }
    }
    crate::recovery::require_retry_choice(db, id)?;
    let worker = task
        .worker_snapshots
        .get(&receiver)
        .ok_or_else(|| Error::Forbidden("接收者不在冻结任务中".into()))?;
    if worker.kind != WorkerKind::Agent {
        return Err(Error::Conflict(
            "人类待办通过正式回应处理，不重试模型运行".into(),
        ));
    }
    let config = worker.execution_config.as_deref().ok_or_else(|| {
        Error::Conflict("冻结执行配置缺失；pending 显式刷新，已承接任务需取消后新建".into())
    })?;
    let configuration: crate::connection::ExecutionConfiguration =
        load(db, "execution_configs", config)?;
    if configuration.worker_id != receiver {
        return Err(Error::Forbidden("执行配置不属于接收者".into()));
    }
    let team: Team = load(db, "teams", &task.team_id)?;
    let permission = match kind.as_str() {
        "assignment.execute" | "assignment.rework" => Permission::Execute,
        "handoff.verify" => Permission::Verify,
        _ if receiver == task.team_snapshot.leader => Permission::Arrange,
        _ => Permission::Communicate,
    };
    for p in [Permission::Communicate, permission] {
        require(&team, &receiver, p.clone())?;
        require(&task.team_snapshot, &receiver, p)?;
    }
    if task.runs_used >= task.contract.max_runs {
        return Err(Error::Conflict("总 Run 额度已耗尽，重试不重置额度".into()));
    }
    if kind == "handoff.verify" {
        crate::handoff::claimable(db, &task, id)?;
    }
    if kind == "assignment.rework" {
        crate::rework::claimable(db, &task, id)?;
    }
    if matches!(
        kind.as_str(),
        "assignment.execute" | "assignment.rework" | "handoff.verify"
    ) {
        crate::blocker::ensure_clear(db, &task)?;
    }
    db.execute("UPDATE deliveries SET status='queued',reason=?2,claim_epoch=NULL,revision=revision+1 WHERE id=?1",params![id,reason])?;
    crate::recovery::resolve_retried(db, actor, id, cause)?;
    Ok(response("queued", rev + 1))
}

pub(crate) fn list(db: &Connection, task: &Task, actor: &str) -> Result<Value> {
    let leader = task.team_snapshot.leader == actor;
    let mut stmt=db.prepare("SELECT d.id,d.revision,d.status,substr(d.reason,1,256),d.receiver,d.run_id,m.kind,m.task_revision,coalesce(length(d.reason)>256,0) FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE m.task_id=?1 AND (?2 OR d.receiver=?3) ORDER BY m.rowid")?;
    let records=stmt.query_map(params![task.id,leader,actor],|r|Ok(json!({"id":r.get::<_,String>(0)?,"revision":r.get::<_,u64>(1)?,"status":r.get::<_,String>(2)?,"reason":r.get::<_,Option<String>>(3)?,"reasonTruncated":r.get::<_,bool>(8)?,"receiver":r.get::<_,String>(4)?,"runId":r.get::<_,Option<String>>(5)?,"kind":r.get::<_,String>(6)?,"messageRevision":r.get::<_,u64>(7)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(json!(records))
}
