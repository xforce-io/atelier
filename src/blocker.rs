//! A reported obstacle ends a member's handling, never proves resources stopped.
use crate::{
    Error, Result,
    model::*,
    store::{Message, Store, enqueue_system, load, new_id, participants, require, revision, text},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Blocker {
    pub id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub run_id: String,
    pub delivery_id: String,
    pub reporter: String,
    pub handler: String,
    pub purpose: String,
    pub reason: String,
    pub state: String,
    pub revision: u64,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
}
fn save(db: &Connection, b: &Blocker) -> Result<()> {
    db.execute("INSERT INTO blockers(id,task_id,run_id,data) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![b.id,b.task_id,b.run_id,serde_json::to_string(b)?])?;
    Ok(())
}
pub(crate) fn for_run(db: &Connection, run: &str) -> Result<Option<Blocker>> {
    let data: Option<String> = db
        .query_row("SELECT data FROM blockers WHERE run_id=?1", [run], |r| {
            r.get(0)
        })
        .optional()?;
    data.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
pub(crate) fn list(db: &Connection, task: &str) -> Result<Value> {
    let mut s = db.prepare("SELECT data FROM blockers WHERE task_id=?1 ORDER BY rowid")?;
    let rows = s
        .query_map([task], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|s| serde_json::from_str::<Value>(&s).map_err(Into::into))
        .collect::<Result<Vec<_>>>()
        .map(|rows| json!(rows))
}
pub(crate) fn ensure_clear(db: &Connection, task: &Task) -> Result<()> {
    let open:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM blockers WHERE task_id=?1 AND json_extract(data,'$.taskRevision')=?2 AND json_extract(data,'$.state')='open')",params![task.id,task.revision],|r|r.get(0))?;
    if open {
        return Err(Error::Conflict(
            "任务仍有未解决的正式阻塞，不能继续安排执行或检验".into(),
        ));
    }
    Ok(())
}
fn can_handle(db: &Connection, task: &Task, actor: &str) -> Result<()> {
    if !participants(task).contains(&actor) {
        return Err(Error::Forbidden("阻塞处理者须参与本任务".into()));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    let p = if actor == task.team_snapshot.acceptor {
        Permission::Manage
    } else {
        Permission::Arrange
    };
    require(&current, actor, p.clone())?;
    require(&task.team_snapshot, actor, p)?;
    require(&current, actor, Permission::Communicate)?;
    require(&task.team_snapshot, actor, Permission::Communicate)
}
pub(crate) fn report(
    db: &Connection,
    run: &Run,
    task: &Task,
    handler: &str,
    reason: &str,
) -> Result<Value> {
    can_handle(db, task, handler)?;
    text(reason, "阻塞原因", 65536)?;
    let b = Blocker {
        id: new_id(),
        task_id: task.id.clone(),
        task_revision: task.revision,
        run_id: run.id.clone(),
        delivery_id: run.delivery_id.clone(),
        reporter: run.worker_id.clone(),
        handler: handler.into(),
        purpose: run.purpose.clone(),
        reason: reason.into(),
        state: "open".into(),
        revision: 1,
        resolution: None,
        resolved_by: None,
    };
    save(db, &b)?;
    let mut updated = task.clone();
    let mut recipients = vec![handler.to_string(), task.team_snapshot.leader.clone()];
    // A leader reporting its own obstacle escalates to the human, not itself.
    if run.worker_id == task.team_snapshot.leader {
        recipients.push(task.team_snapshot.acceptor.clone());
    }
    recipients.retain(|id| id != &run.worker_id);
    if recipients.is_empty() {
        recipients.push(task.team_snapshot.acceptor.clone());
    }
    recipients.sort();
    recipients.dedup();
    for recipient in recipients {
        enqueue_system(db,&mut updated,&run.id,Message {recipient:&recipient,kind:"blocker",body:&json!({"blockerId":b.id,"runId":run.id,"handler":handler,"reason":reason,"next":"先核对停止，再记录原契约范围内的修复依据；此报告不代表资源已停止"}).to_string(),reply_to:None,event:Some(&format!("blocker:{}:{recipient}",b.id)),mandatory:true})?;
    }
    crate::disposition::record(
        db,
        run,
        &updated,
        json!({"kind":"blocker","blockerId":b.id}),
    )?;
    let mut stopped = run.clone();
    stopped.stop_requested = true;
    stopped.stop_reason = Some(format!("成员报告阻塞：{}", b.id));
    crate::runs::save(db, &stopped)?;
    Ok(
        json!({"blocker":b,"stopRequested":true,"resourcesStopped":false,"next":"结束本轮；运行服务核对资源后才释放 Run。停止后通过 blocker show 查询保存事实"}),
    )
}
pub(crate) fn authorize_resolve(
    db: &Connection,
    task: &Task,
    b: &Blocker,
    actor: &str,
) -> Result<()> {
    if actor != task.team_snapshot.acceptor && actor != b.handler {
        return Err(Error::Forbidden(
            "仅本人或指定的获准协调者可以解决阻塞".into(),
        ));
    }
    can_handle(db, task, actor)
}
pub(crate) fn resolve(
    db: &Connection,
    actor: &str,
    cause: &str,
    id: &str,
    expected: u64,
    task_revision: u64,
    evidence: &str,
) -> Result<Value> {
    let mut b: Blocker = load(db, "blockers", id)?;
    let mut task: Task = load(db, "tasks", &b.task_id)?;
    authorize_resolve(db, &task, &b, actor)?;
    revision(b.revision, expected)?;
    revision(task.revision, task_revision)?;
    if b.state != "open"
        || b.task_revision != task.revision
        || task.state == "closed"
        || task.cancellation_requested
    {
        return Err(Error::Conflict("阻塞已处理、依据过期或任务已取消".into()));
    }
    text(evidence, "原契约范围内的修复依据", 65536)?;
    let source: Run = load(db, "runs", &b.run_id)?;
    let busy:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE task_id=?1 AND state!='stopped' AND NOT(worker_id=?2 AND state='running' AND json_extract(data,'$.purpose')='coordinate'))",params![task.id,actor],|r|r.get(0))?;
    if source.state != "stopped" || busy {
        return Err(Error::Conflict(
            "源 Run 或相关执行资源尚未核对停止，不能记录解决".into(),
        ));
    }
    b.state = "resolved".into();
    b.revision += 1;
    b.resolution = Some(evidence.into());
    b.resolved_by = Some(actor.into());
    save(db, &b)?;
    // Resolution handles corresponding notifications, never requeues the source.
    db.execute("UPDATE deliveries SET status='handled',reason='阻塞已正式解决',revision=revision+1 WHERE status IN ('queued','blocked') AND message_id IN (SELECT id FROM messages WHERE task_id=?1 AND kind='blocker' AND json_extract(body,'$.blockerId')=?2)",params![task.id,b.id])?;
    let leader = task.team_snapshot.leader.clone();
    enqueue_system(db,&mut task,cause,Message {recipient:&leader,kind:"resolved",body:&json!({"blockerId":b.id,"runId":b.run_id,"purpose":b.purpose,"resolution":evidence,"next":"由团队负责人按原职责与预算明确安排返工或重新检验；解决记录不自动运行"}).to_string(),reply_to:None,event:Some(&format!("blocker-resolved:{}:{leader}",b.id)),mandatory:true})?;
    Ok(json!(b))
}
pub(crate) fn resolved_for(db: &Connection, task: &Task, id: &str) -> Result<Blocker> {
    let b: Blocker = load(db, "blockers", id)?;
    let run: Run = load(db, "runs", &b.run_id)?;
    if b.task_id != task.id
        || b.task_revision != task.revision
        || b.state != "resolved"
        || run.state != "stopped"
    {
        return Err(Error::Conflict(
            "必须引用同任务、同契约且资源已停止的已解决阻塞".into(),
        ));
    }
    Ok(b)
}
pub(crate) fn supersede(db: &Connection, task: &str, reason: &str) -> Result<()> {
    db.execute("UPDATE blockers SET data=json_set(data,'$.state','superseded','$.revision',json_extract(data,'$.revision')+1,'$.resolution',?2) WHERE task_id=?1 AND json_extract(data,'$.state')='open'",params![task,reason])?;
    db.execute("UPDATE deliveries SET status='cancelled',reason=?2,revision=revision+1 WHERE status IN ('queued','blocked') AND message_id IN (SELECT id FROM messages WHERE task_id=?1 AND kind='blocker')",params![task,reason])?;
    Ok(())
}
impl Store {
    pub fn blocker(&self, id: &str) -> Result<Blocker> {
        load(&self.connection, "blockers", id)
    }
    pub fn blockers(&self, task: &str) -> Result<Value> {
        self.task(task)?;
        list(&self.connection, task)
    }
    /// Resource-owner only, after all writers, children and accepted calls drain.
    /// Only the controlled manifest can prove no output; an absent native directory cannot.
    pub(crate) fn runtime_finish_empty_blocked_execution(
        &mut self,
        epoch: &str,
        id: &str,
        reason: &str,
    ) -> Result<bool> {
        text(reason, "资源停止核对依据", 65536)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, false)?;
        let mut run: Run = load(&tx, "runs", id)?;
        if !matches!(run.purpose.as_str(), "execute" | "rework")
            || for_run(&tx, id)?.is_none()
            || !crate::candidate::candidate(&tx, id)?.is_some_and(|c| c.files.is_empty())
        {
            return Ok(false);
        }
        let published: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM artifacts WHERE run_id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if published {
            return Ok(false);
        }
        if run.state == "stopped" {
            return Ok(true);
        }
        // A new service may reconcile an old unknown Run only with its resource-owner proof.
        if run.epoch != epoch && run.state != "unknown" {
            return Err(Error::Conflict("不能核对其它服务仍持有的 Run".into()));
        }
        crate::runs::stopped(&tx, &mut run, reason)?;
        tx.commit()?;
        Ok(true)
    }
}
