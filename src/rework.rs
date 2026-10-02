//! Explicit rework arrangements bind a stopped predecessor and immutable input.
use crate::{
    Error, Result,
    artifact::Artifact,
    model::*,
    store::{Message, enqueue, load, require, revision, text},
    verification::Verification,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReworkReason {
    Verification { id: String },
    Blocker { id: String },
    Rejection { id: String },
    RunFailure { id: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Rework {
    pub id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub previous_run: String,
    pub base_artifact: Option<String>,
    pub prior_current_artifact: Option<String>,
    pub reason: ReworkReason,
}
/// Unclaimed blocked work still reserves its slot; cancellation releases it.
pub(crate) fn reserved(db: &Connection, task: &str) -> Result<u32> {
    Ok(db.query_row("SELECT count(*) FROM reworks r JOIN deliveries d ON d.id=r.id WHERE r.task_id=?1 AND d.run_id IS NULL AND d.status IN ('queued','blocked')", [task], |r| r.get(0))?)
}
pub(crate) fn list(db: &Connection, task: &str) -> Result<Value> {
    let mut s = db.prepare("SELECT r.data,d.status,d.run_id FROM reworks r JOIN deliveries d ON d.id=r.id WHERE r.task_id=?1 ORDER BY r.rowid")?;
    let rows = s
        .query_map([task], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(data, status, run)| {
            let mut value: Value = serde_json::from_str(&data)?;
            value["budgetState"] = json!(if run.is_some() {
                "consumed"
            } else if status == "cancelled" {
                "released"
            } else {
                "reserved"
            });
            value["deliveryStatus"] = json!(status);
            value["runId"] = json!(run);
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()
        .map(|rows| json!(rows))
}
fn latest_execution(db: &Connection, task: &str) -> Result<Run> {
    let data: Option<String> = db.query_row("SELECT data FROM runs WHERE task_id=?1 AND json_extract(data,'$.purpose') IN ('execute','rework') ORDER BY rowid DESC LIMIT 1", [task], |r|r.get(0)).optional()?;
    serde_json::from_str(
        &data.ok_or_else(|| Error::Conflict("首次执行尚未受理，不能安排返工".into()))?,
    )
    .map_err(Into::into)
}
fn basis(db: &Connection, task: &Task, reason: &ReworkReason) -> Result<(Run, Option<String>)> {
    let previous = latest_execution(db, &task.id)?;
    if previous.state != "stopped" || previous.task_revision != task.revision {
        return Err(Error::Conflict("前次执行尚未核对停止或契约已变化".into()));
    }
    match reason {
        ReworkReason::Rejection { id } => {
            let d: DecisionRequest = load(db, "decisions", id)?;
            let record: crate::acceptance::AcceptanceDecision =
                load(db, "acceptance_decisions", id)?;
            let a: Artifact = load(db, "artifacts", &record.artifact_id)?;
            if d.kind != "acceptance"
                || d.state != "rejected"
                || d.task_id != task.id
                || d.task_revision != task.revision
                || record.accepted
                || record.task_id != task.id
                || a.run_id != previous.id
                || task.current_artifact.as_deref() != Some(a.id.as_str())
            {
                return Err(Error::Conflict(
                    "返工须引用当前产出的正式人类拒绝记录".into(),
                ));
            }
            Ok((previous, Some(a.id)))
        }
        ReworkReason::Blocker { id } => {
            let b = crate::blocker::resolved_for(db, task, id)?;
            if b.run_id != previous.id || !matches!(b.purpose.as_str(), "execute" | "rework") {
                return Err(Error::Conflict("返工须引用最近执行的已解决阻塞".into()));
            }
            let artifact: Option<String> = db
                .query_row(
                    "SELECT id FROM artifacts WHERE run_id=?1",
                    [&previous.id],
                    |r| r.get(0),
                )
                .optional()?;
            let candidate = crate::candidate::candidate(db, &previous.id)?;
            if artifact.is_none()
                && (candidate.as_ref().is_some_and(|c| !c.files.is_empty())
                    || (task.contract.code_input.is_some()
                        && previous.launch_started
                        && candidate.is_none()))
            {
                return Err(Error::Conflict(
                    "先固定原执行的 partial，不能丢弃非空候选".into(),
                ));
            }
            Ok((previous, artifact.or_else(|| task.current_artifact.clone())))
        }
        ReworkReason::Verification { id } => {
            let v: Verification = load(db, "verifications", id)?;
            let a: Artifact = load(db, "artifacts", &v.artifact_id)?;
            let verifier: Run = load(db, "runs", &v.run_id)?;
            if v.task_id != task.id
                || v.task_revision != task.revision
                || v.conclusion != "fail"
                || verifier.state != "stopped"
                || task.current_artifact.as_deref() != Some(a.id.as_str())
                || a.run_id != previous.id
                || a.partial
            {
                return Err(Error::Conflict(
                    "返工须引用当前产出的独立失败检验，且检验资源已停止".into(),
                ));
            }
            Ok((previous, Some(a.id)))
        }
        ReworkReason::RunFailure { id } => {
            let status: String = db.query_row(
                "SELECT status FROM deliveries WHERE id=?1",
                [&previous.delivery_id],
                |r| r.get(0),
            )?;
            if id != &previous.id
                || status != "blocked"
                || crate::disposition::valid_for_stop(db, &previous, task)?
            {
                return Err(Error::Conflict(
                    "须引用本任务最近一次已核对停止且没有业务终局的执行失败".into(),
                ));
            }
            let artifact: Option<String> = db
                .query_row(
                    "SELECT id FROM artifacts WHERE run_id=?1",
                    [&previous.id],
                    |r| r.get(0),
                )
                .optional()?;
            if task.contract.code_input.is_some()
                && artifact.is_none()
                && (previous.launch_started
                    || crate::candidate::candidate(db, &previous.id)?.is_some())
            {
                return Err(Error::Conflict(
                    "先固定前次执行的 partial，不能在返工时丢弃候选内容".into(),
                ));
            }
            Ok((previous, artifact.or_else(|| task.current_artifact.clone())))
        }
    }
}
pub(crate) fn arrange(
    db: &Connection,
    actor: &str,
    cause: &str,
    id: &str,
    expected: u64,
    reason: &ReworkReason,
    instruction: &str,
) -> Result<Value> {
    let mut task: Task = load(db, "tasks", id)?;
    revision(task.revision, expected)?;
    crate::blocker::ensure_clear(db, &task)?;
    if task.state != "active" || task.cancellation_requested {
        return Err(Error::Conflict("只有已承接且未取消的任务可安排返工".into()));
    }
    if actor != task.team_snapshot.leader {
        return Err(Error::Forbidden("只有冻结的团队负责人可以安排返工".into()));
    }
    let team: Team = load(db, "teams", &task.team_id)?;
    for p in [Permission::Arrange, Permission::Communicate] {
        require(&team, actor, p.clone())?;
        require(&task.team_snapshot, actor, p)?;
    }
    text(instruction, "返工说明", 65536)?;
    if task.runs_used >= task.contract.max_runs
        || task.reworks_used + reserved(db, id)? >= task.contract.max_reworks
    {
        return Err(Error::Conflict(
            "任务运行或返工额度耗尽（含已预留），不能安排新返工".into(),
        ));
    }
    // The leader's own coordination Run may arrange work; all other resources
    // must be stopped. Queued/blocked verification also excludes conflicting work.
    let active: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE task_id=?1 AND state!='stopped' AND NOT (worker_id=?2 AND json_extract(data,'$.purpose')='coordinate' AND state='running'))",params![id,actor],|r|r.get(0))?;
    let pending: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM messages m JOIN deliveries d ON d.message_id=m.id WHERE m.task_id=?1 AND m.kind IN ('assignment.execute','assignment.rework','handoff.verify') AND ((d.run_id IS NULL AND d.status IN ('queued','blocked')) OR d.status IN ('claimed','uncertain')))",[id],|r|r.get(0))?;
    if active || pending {
        return Err(Error::Conflict(
            "仍有未结束的执行或检验安排，不能并行安排返工".into(),
        ));
    }
    let (previous, base_artifact) = basis(db, &task, reason)?;
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
            require(&team, &receiver, p.clone()).is_err()
                || require(&task.team_snapshot, &receiver, p.clone()).is_err()
        }) {
        Some("执行成员缺少当前或冻结授权")
    } else if worker.kind == WorkerKind::Agent && worker.execution_config.is_none() {
        Some("执行成员的冻结执行配置缺失")
    } else {
        None
    };
    let body = json!({"action":"rework","instruction":instruction,"reason":reason,"previousRun":previous.id,"baseArtifact":base_artifact,"contractRevision":task.revision,"executor":receiver}).to_string();
    let mut delivery = enqueue(
        db,
        &mut task,
        actor,
        cause,
        Message {
            recipient: &receiver,
            kind: "assignment.rework",
            body: &body,
            reply_to: None,
            event: None,
            mandatory: false,
        },
    )?;
    let rework = Rework {
        id: delivery["deliveryId"].as_str().unwrap().into(),
        task_id: id.into(),
        task_revision: task.revision,
        previous_run: previous.id,
        base_artifact,
        prior_current_artifact: task.current_artifact.clone(),
        reason: reason.clone(),
    };
    db.execute(
        "INSERT INTO reworks(id,task_id,data) VALUES(?1,?2,?3)",
        params![rework.id, id, serde_json::to_string(&rework)?],
    )?;
    if let Some(reason) = blocked {
        db.execute(
            "UPDATE deliveries SET status='blocked',reason=?2 WHERE id=?1",
            params![rework.id, reason],
        )?;
        delivery["status"] = json!("blocked");
        delivery["reason"] = json!(reason);
    }
    Ok(
        json!({"action":"rework","taskId":id,"taskRevision":task.revision,"executor":receiver,"rework":rework,"delivery":delivery,"budget":{"reworksUsed":task.reworks_used,"reworksReserved":reserved(db,id)?},"next":"返工安排及额度预留已保存；领取才消费 Run 与返工额度，不代表已执行"}),
    )
}
pub(crate) fn claimable(db: &Connection, task: &Task, delivery: &str) -> Result<()> {
    crate::blocker::ensure_clear(db, task)?;
    let r: Rework = load(db, "reworks", delivery)?;
    if r.task_id != task.id
        || r.task_revision != task.revision
        || r.prior_current_artifact != task.current_artifact
        || task.reworks_used >= task.contract.max_reworks
    {
        return Err(Error::Conflict("返工依据已变化或额度耗尽".into()));
    }
    let (prior, artifact) = basis(db, task, &r.reason)?;
    if prior.id != r.previous_run || artifact != r.base_artifact {
        return Err(Error::Conflict("返工依据不再是保存的执行与产出".into()));
    }
    Ok(())
}
pub(crate) fn input(db: &Connection, task: &Task, run: &Run) -> Result<Option<Artifact>> {
    let r: Rework = load(db, "reworks", &run.delivery_id)?;
    if r.task_id != task.id
        || r.task_revision != task.revision
        || r.prior_current_artifact != task.current_artifact
    {
        return Err(Error::Conflict("返工输入依据变化".into()));
    }
    r.base_artifact
        .map(|id| load(db, "artifacts", &id))
        .transpose()
}
