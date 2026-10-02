//! Explicit, version-bound handoff decisions. No inspection starts on offer or accept.
use crate::{
    Error, Result,
    artifact::Artifact,
    model::*,
    store::{Message, Store, enqueue, load, new_id, require, revision, text},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Handoff {
    pub prior_state: Option<String>,
    pub superseded_by_blocker: Option<String>,
    pub superseded_by_verification: Option<String>,
    pub id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub artifact_id: String,
    pub sender: String,
    pub receiver: String,
    pub instruction: String,
    pub state: String,
    pub revision: u64,
    pub reason: Option<String>,
    pub delivery_id: String,
}
fn save(db: &Connection, h: &Handoff) -> Result<()> {
    db.execute("INSERT INTO handoffs(id,task_id,artifact_id,receiver,state,data) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET state=excluded.state,data=excluded.data",params![h.id,h.task_id,h.artifact_id,h.receiver,h.state,serde_json::to_string(h)?])?;
    Ok(())
}
fn current(db: &Connection, task: &Task, artifact_id: &str) -> Result<Artifact> {
    crate::blocker::ensure_clear(db, task)?;
    if task.state != "active"
        || task.cancellation_requested
        || task.current_artifact.as_deref() != Some(artifact_id)
    {
        return Err(Error::Conflict(
            "交接必须引用当前未取消任务的固定产出".into(),
        ));
    }
    let artifact: Artifact = load(db, "artifacts", artifact_id)?;
    let latest: Option<String> = db.query_row(
        "SELECT id FROM runs WHERE task_id=?1 AND json_extract(data,'$.purpose') IN ('execute','rework') ORDER BY rowid DESC LIMIT 1",
        [&task.id], |r| r.get(0)).optional()?;
    if crate::rework::reserved(db, &task.id)? != 0
        || latest.as_deref() != Some(artifact.run_id.as_str())
    {
        return Err(Error::Conflict(
            "已有后续返工安排或执行，旧产出不能继续交接或检验".into(),
        ));
    }

    if artifact.task_id != task.id || artifact.task_revision != task.revision || artifact.partial {
        return Err(Error::Conflict(
            "产出范围或版本不匹配，partial 不能作为完整交接".into(),
        ));
    }
    Ok(artifact)
}
pub(crate) fn claimable(db: &Connection, task: &Task, delivery: &str) -> Result<()> {
    let data: String = db.query_row(
        "SELECT data FROM handoffs WHERE json_extract(data,'$.delivery_id')=?1",
        [delivery],
        |r| r.get(0),
    )?;
    let h: Handoff = serde_json::from_str(&data)?;
    if h.task_id != task.id
        || h.task_revision != task.revision
        || !matches!(h.state.as_str(), "offered" | "accepted")
    {
        return Err(Error::Conflict("交接依据已失效或已拒收".into()));
    }
    current(db, task, &h.artifact_id)?;
    Ok(())
}
pub(crate) fn offer(
    db: &Connection,
    actor: &str,
    cause: &str,
    task_id: &str,
    expected: u64,
    artifact_id: &str,
    instruction: &str,
) -> Result<Value> {
    let mut task: Task = load(db, "tasks", task_id)?;
    revision(task.revision, expected)?;
    let artifact = current(db, &task, artifact_id)?;
    let team: Team = load(db, "teams", &task.team_id)?;
    let permission = if actor == task.team_snapshot.leader {
        Permission::Arrange
    } else if task.team_snapshot.executor.as_deref() == Some(actor) && artifact.worker_id == actor {
        Permission::Handoff
    } else {
        return Err(Error::Forbidden(
            "只有团队负责人或获准的原执行成员可交接".into(),
        ));
    };
    for p in [permission, Permission::Communicate] {
        require(&team, actor, p.clone())?;
        require(&task.team_snapshot, actor, p)?;
    }
    text(instruction, "交接说明", 65536)?;
    if task.runs_used >= task.contract.max_runs {
        return Err(Error::Conflict("Run 额度耗尽，不能安排新的检验".into()));
    }
    let receiver = task
        .team_snapshot
        .verifier
        .clone()
        .ok_or_else(|| Error::Invalid("任务缺少冻结检验成员".into()))?;
    if receiver == artifact.worker_id {
        return Err(Error::Forbidden("产出生产者不能独立检验自己的产出".into()));
    }
    let duplicate:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM handoffs WHERE task_id=?1 AND artifact_id=?2 AND receiver=?3 AND state NOT IN ('rejected','superseded'))",params![task.id,artifact_id,receiver],|r|r.get(0))?;
    if duplicate {
        return Err(Error::Conflict(
            "该产出已有未拒收的交接；不能重复送检".into(),
        ));
    }
    let mut h = Handoff {
        prior_state: None,
        superseded_by_blocker: None,
        superseded_by_verification: None,
        id: new_id(),
        task_id: task.id.clone(),
        task_revision: task.revision,
        artifact_id: artifact_id.into(),
        sender: actor.into(),
        receiver: receiver.clone(),
        instruction: instruction.into(),
        state: "offered".into(),
        revision: 1,
        reason: None,
        delivery_id: String::new(),
    };
    let body=json!({"handoffId":h.id,"artifactId":artifact_id,"instruction":instruction,"next":"由指定检验成员明确接收或拒收；接收前不得运行检查"}).to_string();
    let mut delivery = enqueue(
        db,
        &mut task,
        actor,
        cause,
        Message {
            recipient: &receiver,
            kind: "handoff.verify",
            body: &body,
            reply_to: None,
            event: Some(&format!("handoff:{}", h.id)),
            mandatory: false,
        },
    )?;
    h.delivery_id = delivery["deliveryId"].as_str().unwrap().into();
    let worker = task
        .worker_snapshots
        .get(&receiver)
        .ok_or_else(|| Error::Invalid("检验成员快照缺失".into()))?;
    let blocked = if [Permission::Verify, Permission::Communicate]
        .iter()
        .any(|p| {
            require(&team, &receiver, p.clone()).is_err()
                || require(&task.team_snapshot, &receiver, p.clone()).is_err()
        }) {
        Some("检验成员缺少当前或冻结授权")
    } else if worker.kind == WorkerKind::Agent && worker.execution_config.is_none() {
        Some("检验成员的冻结执行配置缺失")
    } else {
        None
    };
    if let Some(reason) = blocked {
        db.execute(
            "UPDATE deliveries SET status='blocked',reason=?2 WHERE id=?1",
            params![h.delivery_id, reason],
        )?;
        delivery["status"] = json!("blocked");
        delivery["reason"] = json!(reason);
    }
    save(db, &h)?;
    Ok(json!({"handoff":h,"delivery":delivery}))
}
pub(crate) fn read(db: &Connection, run: &Run, task: &Task, id: &str) -> Result<Handoff> {
    let h: Handoff = load(db, "handoffs", id)?;
    if h.task_id != task.id
        || ![
            h.sender.as_str(),
            h.receiver.as_str(),
            task.team_snapshot.leader.as_str(),
        ]
        .contains(&run.worker_id.as_str())
    {
        return Err(Error::Forbidden("交接不在本成员当前任务的可见范围".into()));
    }
    Ok(h)
}
pub(crate) fn respond(
    db: &Connection,
    run: &Run,
    task: &Task,
    id: &str,
    expected: u64,
    accept: bool,
    reason: &str,
) -> Result<Value> {
    let mut h = read(db, run, task, id)?;
    revision(h.revision, expected)?;
    text(reason, "接收决定依据", 65536)?;
    if run.purpose != "verify"
        || h.receiver != run.worker_id
        || h.delivery_id != run.delivery_id
        || !run.permissions.contains(&Permission::Verify)
    {
        return Err(Error::Forbidden(
            "只能由持有本交接投递的指定检验成员作决定".into(),
        ));
    }
    require(
        &load::<Team>(db, "teams", &task.team_id)?,
        &run.worker_id,
        Permission::Verify,
    )?;
    current(db, task, &h.artifact_id)?;
    if h.state != "offered" {
        return Err(Error::Conflict(
            "交接已有接收决定，不可用新请求重复决定".into(),
        ));
    }
    h.state = if accept { "accepted" } else { "rejected" }.into();
    h.revision += 1;
    h.reason = Some(reason.into());
    save(db, &h)?;
    if !accept {
        let mut task = task.clone();
        enqueue(db,&mut task,&run.worker_id,&run.id,Message {recipient:&h.sender,kind:"result",body:&json!({"handoffId":h.id,"artifactId":h.artifact_id,"state":"rejected","reason":reason,"next":"原范围补充后可重新交接；未运行检查，没有检验通过结论"}).to_string(),reply_to:None,event:Some(&format!("handoff-rejected:{}",h.id)),mandatory:true})?;
        crate::disposition::record(
            db,
            run,
            &task,
            json!({"kind":"handoff_rejected","handoffId":h.id,"reason":reason}),
        )?;
    }
    Ok(
        json!({"handoff":h,"next":if accept {"交接已接受；须运行获准检查并提交检验建议，接收本身不是检验结论"} else {"结束本轮；核对资源停止后处理投递，不运行检查"}}),
    )
}
pub(crate) fn list(db: &Connection, task: &str) -> Result<Value> {
    let mut statement = db.prepare("SELECT data FROM handoffs WHERE task_id=?1 ORDER BY rowid")?;
    let data = statement
        .query_map([task], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let records = data
        .into_iter()
        .map(|s| serde_json::from_str::<Handoff>(&s))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!(records.into_iter().map(|h|json!({"id":h.id,"artifactId":h.artifact_id,"sender":h.sender,"receiver":h.receiver,"state":h.state,"revision":h.revision})).collect::<Vec<_>>()))
}
impl Store {
    pub fn handoff(&self, id: &str) -> Result<Handoff> {
        load(&self.connection, "handoffs", id)
    }
}

pub(crate) fn offer_again(
    db: &Connection,
    actor: &str,
    cause: &str,
    command: &Command,
) -> Result<Value> {
    let Command::TaskVerify {
        blocker_id,
        verification_id,
        id: task_id,
        revision: expected,
        artifact_id,
        instruction,
    } = command
    else {
        return Err(Error::Invalid("不是重新检验操作".into()));
    };
    let task: Task = load(db, "tasks", task_id)?;
    revision(task.revision, *expected)?;
    if actor != task.team_snapshot.leader {
        return Err(Error::Forbidden("仅团队负责人可安排重新检验".into()));
    }
    let run_id = match (blocker_id, verification_id) {
        (Some(id), None) => {
            let b = crate::blocker::resolved_for(db, &task, id)?;
            if b.purpose != "verify" {
                return Err(Error::Conflict("该阻塞不是独立检验阻塞".into()));
            }
            b.run_id
        }
        (None, Some(id)) => {
            let v: crate::verification::Verification = load(db, "verifications", id)?;
            let latest:Option<String>=db.query_row("SELECT id FROM verifications WHERE task_id=?1 AND artifact_id=?2 ORDER BY rowid DESC LIMIT 1",params![task.id,artifact_id],|r|r.get(0)).optional()?;
            if v.task_id != task.id
                || v.task_revision != task.revision
                || &v.artifact_id != artifact_id
                || v.conclusion != "inconclusive"
                || latest.as_deref() != Some(v.id.as_str())
            {
                return Err(Error::Conflict(
                    "重新检验须引用当前产出最新的 inconclusive 检验，不能绕过明确失败".into(),
                ));
            }
            v.run_id
        }
        _ => {
            return Err(Error::Invalid(
                "重新检验须且仅须引用已解决阻塞或 inconclusive 检验".into(),
            ));
        }
    };
    let run: Run = load(db, "runs", &run_id)?;
    let other_active:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE task_id=?1 AND state!='stopped' AND NOT(state='running' AND json_extract(data,'$.purpose')='coordinate' AND worker_id=?2))",params![task.id,actor],|r|r.get(0))?;
    if run.task_id != task.id
        || run.task_revision != task.revision
        || run.purpose != "verify"
        || run.state != "stopped"
        || other_active
    {
        return Err(Error::Conflict(
            "原检验及其它相关资源须核对停止后才能重新交接".into(),
        ));
    }
    let data: String = db.query_row(
        "SELECT data FROM handoffs WHERE json_extract(data,'$.delivery_id')=?1",
        [&run.delivery_id],
        |r| r.get(0),
    )?;
    let mut old: Handoff = serde_json::from_str(&data)?;
    if old.task_id != task.id
        || old.task_revision != task.revision
        || &old.artifact_id != artifact_id
        || !matches!(old.state.as_str(), "offered" | "accepted")
    {
        return Err(Error::Conflict("旧交接已替代或产出依据不同".into()));
    }
    old.prior_state = Some(old.state.clone());
    old.superseded_by_blocker = blocker_id.clone();
    old.superseded_by_verification = verification_id.clone();
    old.state = "superseded".into();
    old.revision += 1;
    save(db, &old)?;
    let mut result = offer(
        db,
        actor,
        cause,
        task_id,
        *expected,
        artifact_id,
        instruction,
    )?;
    result["resolvedBlockerId"] = json!(blocker_id);
    result["inconclusiveVerificationId"] = json!(verification_id);
    result["supersededHandoffId"] = json!(old.id);
    Ok(result)
}
