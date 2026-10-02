//! An arrangement is a durable business message, never an executor launch.
use crate::{
    Error, Result,
    model::*,
    store::{Message, enqueue, load, require, revision, text},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

pub(crate) fn list(db: &Connection, task: &str) -> Result<Value> {
    let mut statement = db.prepare("SELECT m.id,d.id,m.sender,m.recipient,m.task_revision,d.status,d.reason,d.run_id FROM messages m JOIN deliveries d ON d.message_id=m.id WHERE m.task_id=?1 AND m.kind IN ('assignment.execute','assignment.rework') ORDER BY m.rowid")?;
    let rows = statement.query_map([task], |r| Ok(json!({"messageId":r.get::<_,String>(0)?,"deliveryId":r.get::<_,String>(1)?,"sender":r.get::<_,String>(2)?,"executor":r.get::<_,String>(3)?,"taskRevision":r.get::<_,u64>(4)?,"status":r.get::<_,String>(5)?,"reason":r.get::<_,Option<String>>(6)?,"runId":r.get::<_,Option<String>>(7)?})))?.collect::<std::result::Result<Vec<_>,_>>()?;
    Ok(json!(rows))
}

pub(crate) fn execute(
    db: &Connection,
    actor: &str,
    cause: &str,
    id: &str,
    expected: u64,
    instruction: &str,
) -> Result<Value> {
    let mut task: Task = load(db, "tasks", id)?;
    revision(task.revision, expected)?;
    crate::blocker::ensure_clear(db, &task)?;
    if task.state != "active" || task.cancellation_requested {
        return Err(Error::Conflict(
            "只有已承接且未取消的任务可以安排执行".into(),
        ));
    }
    if task.team_snapshot.leader != actor {
        return Err(Error::Forbidden("只有冻结的团队负责人可以安排执行".into()));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    for permission in [Permission::Arrange, Permission::Communicate] {
        require(&current, actor, permission.clone())?;
        require(&task.team_snapshot, actor, permission)?;
    }
    text(instruction, "执行说明", 65536)?;
    if task.runs_used >= task.contract.max_runs {
        return Err(Error::Conflict("任务 Run 额度耗尽，不能安排新执行".into()));
    }
    let started: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE task_id=?1 AND json_extract(data,'$.purpose') IN ('execute','rework'))", [id], |r| r.get(0))?;
    if started {
        return Err(Error::Conflict(
            "首次执行已经受理；后续代码重做必须安排返工".into(),
        ));
    }
    let pending: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM messages m JOIN deliveries d ON d.message_id=m.id WHERE m.task_id=?1 AND m.kind IN ('assignment.execute','assignment.rework') AND d.status!='cancelled')", [id], |r| r.get(0))?;
    if pending {
        return Err(Error::Conflict(
            "本任务已有执行安排，不能通过新请求重复安排".into(),
        ));
    }
    let receiver = task
        .team_snapshot
        .executor
        .clone()
        .ok_or_else(|| Error::Invalid("任务缺少冻结执行成员".into()))?;
    let worker = task
        .worker_snapshots
        .get(&receiver)
        .ok_or_else(|| Error::Invalid("执行成员快照缺失".into()))?;
    let blocked = if [Permission::Execute, Permission::Communicate]
        .iter()
        .any(|p| {
            require(&current, &receiver, p.clone()).is_err()
                || require(&task.team_snapshot, &receiver, p.clone()).is_err()
        }) {
        Some("执行成员缺少当前或冻结授权")
    } else if worker.kind == WorkerKind::Agent && worker.execution_config.is_none() {
        Some("执行成员的冻结执行配置缺失")
    } else {
        None
    };
    let body = json!({"action":"execute","instruction":instruction,"contractRevision":task.revision,"executor":receiver}).to_string();
    let mut delivery = enqueue(
        db,
        &mut task,
        actor,
        cause,
        Message {
            recipient: &receiver,
            kind: "assignment.execute",
            body: &body,
            reply_to: None,
            event: None,
            mandatory: false,
        },
    )?;
    if let Some(reason) = blocked {
        db.execute(
            "UPDATE deliveries SET status='blocked',reason=?2 WHERE id=?1",
            params![delivery["deliveryId"].as_str().unwrap(), reason],
        )?;
        delivery["status"] = json!("blocked");
        delivery["reason"] = json!(reason);
    }
    Ok(
        json!({"action":"execute","taskId":task.id,"taskRevision":task.revision,"executor":receiver,"delivery":delivery,"next":"安排已持久保存；是否实际启动及产出须另查 Run，不表示已执行"}),
    )
}
