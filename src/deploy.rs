//! Deploy duty is an explicit team role. The core records the result and never
//! runs compose, merges, or pushes.
use crate::{
    Error, Result,
    model::*,
    store::{self, Message, enqueue_system, load, require, revision, text},
};
use rusqlite::Connection;
use serde_json::{Value, json};

pub(crate) fn open_after_acceptance(
    db: &Connection,
    task: &mut Task,
    acceptance_id: &str,
    cause: &str,
) -> Result<()> {
    let deployer = task
        .team_snapshot
        .deployer
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有冻结部署职责".into()))?;
    let worker = task
        .worker_snapshots
        .get(&deployer)
        .cloned()
        .ok_or_else(|| Error::Conflict("部署成员快照缺失".into()))?;
    if let Some(reason) = block_reason(db, task, &deployer, &worker)? {
        task.deploy = Some(DeployRecord {
            state: "blocked".into(),
            acceptance_id: acceptance_id.into(),
            reason: Some(reason),
        });
        task.revision += 1;
        return store::save_task(db, task);
    }
    task.deploy = Some(DeployRecord {
        state: "open".into(),
        acceptance_id: acceptance_id.into(),
        reason: None,
    });
    task.revision += 1;
    let body = json!({
        "acceptanceId": acceptance_id,
        "instruction": "提交本次部署结果。核心不执行部署、不合入、不 push。"
    })
    .to_string();
    enqueue_system(
        db,
        task,
        cause,
        Message {
            recipient: &deployer,
            kind: "assignment.deploy",
            body: &body,
            reply_to: None,
            event: Some(&format!("deploy:{acceptance_id}")),
            mandatory: false,
        },
    )?;
    Ok(())
}

fn block_reason(
    db: &Connection,
    task: &Task,
    deployer: &str,
    worker: &Worker,
) -> Result<Option<String>> {
    if worker.kind == WorkerKind::Agent && task.runs_used >= task.contract.max_runs {
        return Ok(Some("任务 Run 额度耗尽，不能发起部署".into()));
    }
    if task.messages_used >= task.contract.max_messages {
        return Ok(Some("任务消息额度耗尽，不能发起部署".into()));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    for permission in [Permission::Deploy, Permission::Communicate] {
        if require(&current, deployer, permission.clone()).is_err()
            || require(&task.team_snapshot, deployer, permission).is_err()
        {
            return Ok(Some("部署成员缺少当前或冻结的 task.deploy 授权".into()));
        }
    }
    if worker.kind == WorkerKind::Agent && worker.execution_config.is_none() {
        return Ok(Some("部署成员的冻结执行配置缺失".into()));
    }
    if let Err(error) = crate::blocker::ensure_clear(db, task) {
        return Ok(Some(error.to_string()));
    }
    Ok(None)
}

pub(crate) fn report(
    db: &Connection,
    actor: &str,
    task_id: &str,
    expected: u64,
    result: DeployResult,
    reason: &str,
) -> Result<Value> {
    let mut task: Task = load(db, "tasks", task_id)?;
    revision(task.revision, expected)?;
    text(reason, "部署结果依据", 65536)?;
    if task.cancellation_requested || task.state != "active" {
        return Err(Error::Conflict("任务已关闭或正在取消".into()));
    }
    let deployer = task
        .team_snapshot
        .deployer
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有冻结部署职责".into()))?;
    if actor != deployer {
        return Err(Error::Forbidden("只能由冻结的部署成员提交部署结果".into()));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    require(&current, actor, Permission::Deploy)?;
    require(&task.team_snapshot, actor, Permission::Deploy)?;
    let record = task
        .deploy
        .clone()
        .ok_or_else(|| Error::Conflict("部署尚未打开".into()))?;
    if record.state != "open" {
        return Err(Error::Conflict("部署结果已结束或尚未开放".into()));
    }
    let worker = task
        .worker_snapshots
        .get(actor)
        .ok_or_else(|| Error::Conflict("部署成员快照缺失".into()))?;
    reporting_run(db, &task, actor, &worker.kind)?;
    let mut deploy = record;
    deploy.reason = Some(reason.into());
    match result {
        DeployResult::Succeeded => {
            deploy.state = "succeeded".into();
            task.deploy = Some(deploy);
            task.state = "closed".into();
            task.outcome = Some("deployed".into());
            task.revision += 1;
            crate::decisions::supersede(
                db,
                &task.id,
                None,
                task.revision,
                "部署结果已记录，任务关闭",
            )?;
            crate::blocker::supersede(db, &task.id, "部署结果已记录，任务关闭")?;
            db.execute("UPDATE deliveries SET status='cancelled',reason='部署结果已记录，任务关闭',revision=revision+1 WHERE status IN ('queued','blocked') AND message_id IN (SELECT id FROM messages WHERE task_id=?1)", [&task.id])?;
            store::save_task(db, &task)?;
        }
        DeployResult::Failed => {
            deploy.state = "failed".into();
            task.deploy = Some(deploy);
            task.revision += 1;
            let leader = task.team_snapshot.leader.clone();
            let body =
                json!({"result":"failed","reason":reason,"next":"任务未完成；本版不自动再次部署"})
                    .to_string();
            enqueue_system(
                db,
                &mut task,
                &format!("deploy-failed:{task_id}"),
                Message {
                    recipient: &leader,
                    kind: "failure",
                    body: &body,
                    reply_to: None,
                    event: Some(&format!("deploy-failed:{task_id}")),
                    mandatory: true,
                },
            )?;
        }
    }
    Ok(json!({"task": task, "deploy": task.deploy}))
}

fn reporting_run(db: &Connection, task: &Task, actor: &str, kind: &WorkerKind) -> Result<()> {
    let mut statement =
        db.prepare("SELECT data FROM runs WHERE task_id=?1 AND state!='stopped'")?;
    let runs = statement
        .query_map([&task.id], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let runs = runs
        .iter()
        .map(|data| serde_json::from_str::<Run>(data))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    match kind {
        WorkerKind::Human => {
            if !runs.is_empty() {
                return Err(Error::Conflict("仍有活动 Run，不能提交部署结果".into()));
            }
        }
        WorkerKind::Agent => {
            let current = (runs.len() == 1).then(|| &runs[0]);
            if current.is_none_or(|run| {
                run.worker_id != actor || run.purpose != "deploy" || run.state != "running"
            }) {
                return Err(Error::Forbidden(
                    "数字员工须在自己的部署运行中提交结果".into(),
                ));
            }
        }
    }
    Ok(())
}
