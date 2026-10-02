//! Human acceptance is a version-bound decision, separate from model verification.
use crate::{
    Error, Result,
    artifact::Artifact,
    model::*,
    store::{
        Message, Store, enqueue, enqueue_system, load, new_id, require, revision, save_task, text,
    },
    verification::{Check, CheckTarget, Verification},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceBasis {
    pub artifact_id: String,
    pub verification_id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceDecision {
    pub id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub artifact_id: String,
    pub verification_id: String,
    pub actor: String,
    pub accepted: bool,
    pub reason: String,
    pub decision_ref: Option<String>,
}
pub(crate) fn save_request(db: &Connection, d: &DecisionRequest) -> Result<()> {
    db.execute("INSERT INTO decisions(id,task_id,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![d.id,d.task_id,serde_json::to_string(d)?])?;
    Ok(())
}
pub(crate) fn authorize_request(db: &Connection, task: &Task, actor: &str) -> Result<()> {
    if task.team_snapshot.leader != actor {
        return Err(Error::Forbidden("仅冻结团队负责人可请求人工验收".into()));
    }
    let team: Team = load(db, "teams", &task.team_id)?;
    for p in [Permission::Arrange, Permission::Communicate] {
        require(&team, actor, p.clone())?;
        require(&task.team_snapshot, actor, p)?;
    }
    Ok(())
}
pub(crate) fn authorize_decision(db: &Connection, task: &Task, actor: &str) -> Result<()> {
    if task.team_snapshot.acceptor != actor
        || load::<Worker>(db, "workers", actor)?.kind != WorkerKind::Human
    {
        return Err(Error::Forbidden(
            "只有冻结且获准的人类验收者能接受或拒绝交付".into(),
        ));
    }
    let team: Team = load(db, "teams", &task.team_id)?;
    for p in [Permission::Accept, Permission::Communicate] {
        require(&team, actor, p.clone())?;
        require(&task.team_snapshot, actor, p)?;
    }
    Ok(())
}
fn evidence(db: &Connection, task: &Task, b: &AcceptanceBasis) -> Result<()> {
    if task.state != "active"
        || task.cancellation_requested
        || task.current_artifact.as_deref() != Some(b.artifact_id.as_str())
    {
        return Err(Error::Conflict(
            "验收须引用当前 active 任务的当前产出".into(),
        ));
    }
    crate::blocker::ensure_clear(db, task)?;
    let a: Artifact = load(db, "artifacts", &b.artifact_id)?;
    let v: Verification = load(db, "verifications", &b.verification_id)?;
    let c: Check = load(db, "checks", &v.check_id)?;
    let producer: Run = load(db, "runs", &a.run_id)?;
    let verifier: Run = load(db, "runs", &v.run_id)?;
    let latest_run:Option<String>=db.query_row("SELECT id FROM runs WHERE task_id=?1 AND json_extract(data,'$.purpose') IN ('execute','rework') ORDER BY rowid DESC LIMIT 1",[&task.id],|r|r.get(0)).optional()?;
    let latest_verification:Option<String>=db.query_row("SELECT id FROM verifications WHERE task_id=?1 AND artifact_id=?2 ORDER BY rowid DESC LIMIT 1",params![task.id,a.id],|r|r.get(0)).optional()?;
    if a.partial
        || a.task_id != task.id
        || a.task_revision != task.revision
        || latest_run.as_deref() != Some(a.run_id.as_str())
        || producer.state != "stopped"
        || task.team_snapshot.executor.as_deref() != Some(a.worker_id.as_str())
        || v.task_id != task.id
        || v.task_revision != task.revision
        || v.artifact_id != a.id
        || v.conclusion != "pass"
        || latest_verification.as_deref() != Some(v.id.as_str())
        || verifier.state != "stopped"
        || verifier.purpose != "verify"
        || verifier.worker_id != v.worker_id
        || task.team_snapshot.verifier.as_deref() != Some(v.worker_id.as_str())
        || v.worker_id == a.worker_id
        || c.run_id != v.run_id
        || c.task_id != task.id
        || c.task_revision != task.revision
        || c.state != "finished"
        || !c.resources_stopped
        || c.conclusion.as_deref() != Some("pass")
        || c.content_digest != a.content_digest
        || Some(c.profile_id.as_str()) != task.contract.verification_profile.as_deref()
        || !matches!(&c.target,CheckTarget::Artifact{artifact_id} if artifact_id==&a.id)
    {
        return Err(Error::Conflict(
            "缺少匹配当前契约、产出和独立成员的有效通过证据".into(),
        ));
    }
    Ok(())
}
fn idle(db: &Connection, task: &Task, requester: Option<&str>) -> Result<()> {
    let mut stmt = db.prepare("SELECT data FROM runs WHERE task_id=?1 AND state!='stopped'")?;
    let runs = stmt
        .query_map([&task.id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for data in runs {
        let run: Run = serde_json::from_str(&data)?;
        if requester != Some(run.worker_id.as_str())
            || run.purpose != "coordinate"
            || run.state != "running"
        {
            return Err(Error::Conflict(
                "任务仍有活动或未知 Run，须先核对资源停止".into(),
            ));
        }
    }
    let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM messages m JOIN deliveries d ON d.message_id=m.id WHERE m.task_id=?1 AND m.kind IN ('assignment.execute','assignment.rework','handoff.verify') AND d.status IN ('queued','blocked','claimed','uncertain'))",[&task.id],|r|r.get(0))?;
    if pending {
        return Err(Error::Conflict(
            "仍有待生效执行、返工或检验安排，不能验收旧交付".into(),
        ));
    }
    Ok(())
}
pub(crate) fn apply(db: &Connection, actor: &str, cause: &str, command: &Command) -> Result<Value> {
    match command {
        Command::AcceptanceRequest {
            task_id,
            revision: expected,
            artifact_id,
            verification_id,
            summary,
        } => {
            let mut task: Task = load(db, "tasks", task_id)?;
            revision(task.revision, *expected)?;
            authorize_request(db, &task, actor)?;
            text(summary, "提交验收说明", 65536)?;
            let basis = AcceptanceBasis {
                artifact_id: artifact_id.clone(),
                verification_id: verification_id.clone(),
            };
            evidence(db, &task, &basis)?;
            idle(db, &task, Some(actor))?;
            let rejected:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE task_id=?1 AND json_extract(data,'$.kind')='acceptance' AND json_extract(data,'$.acceptance.artifact_id')=?2 AND json_extract(data,'$.state')='rejected')",params![task.id,artifact_id],|r|r.get(0))?;
            if rejected {
                return Err(Error::Conflict(
                    "该产出已被人类拒绝；必须返工新产出、独立重验后才能重新请求".into(),
                ));
            }
            let open:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE task_id=?1 AND json_extract(data,'$.kind')='acceptance' AND json_extract(data,'$.state')='open')",[task_id],|r|r.get(0))?;
            if open {
                return Err(Error::Conflict(
                    "该任务已有当前验收请求，不能换请求 ID 重复创建".into(),
                ));
            }
            let id = new_id();
            let human = task.team_snapshot.acceptor.clone();
            let body=json!({"decisionId":id,"kind":"acceptance","artifactId":artifact_id,"verificationId":verification_id,"contractVersion":task.revision,"summary":summary,"next":"本人明确接受或拒绝；普通回复和已读不等于验收"}).to_string();
            let delivery = enqueue(
                db,
                &mut task,
                actor,
                cause,
                Message {
                    recipient: &human,
                    kind: "decision.request",
                    body: &body,
                    reply_to: None,
                    event: Some(&format!("acceptance:{id}")),
                    mandatory: false,
                },
            )?;
            let d = DecisionRequest {
                recovery: None,
                acceptance: Some(basis),
                id,
                task_id: task.id.clone(),
                task_revision: task.revision,
                effective_revision: task.revision,
                requester: actor.into(),
                handler: human,
                kind: "acceptance".into(),
                question: summary.into(),
                impact: "接受将关闭任务；拒绝保留历史且同一产出不能重新请求验收".into(),
                options: vec!["accept".into(), "reject".into()],
                revision: 1,
                state: "open".into(),
                answer: None,
                blocked_reason: None,
                reason: None,
                operation: None,
                request_delivery: delivery["deliveryId"].as_str().unwrap().into(),
                response_delivery: None,
            };
            save_request(db, &d)?;
            Ok(json!({"decision":d,"delivery":delivery}))
        }
        Command::AcceptanceDecide {
            task_id,
            revision: expected,
            request_id,
            request_revision,
            accept,
            reason,
            decision_ref,
        } => {
            let mut task: Task = load(db, "tasks", task_id)?;
            authorize_decision(db, &task, actor)?;
            revision(task.revision, *expected)?;
            let mut d: DecisionRequest = load(db, "decisions", request_id)?;
            revision(d.revision, *request_revision)?;
            if d.task_id != task.id
                || d.kind != "acceptance"
                || d.state != "open"
                || d.task_revision != task.revision
                || d.handler != actor
            {
                return Err(Error::Conflict(
                    "只能回应当前版本尚未决定的本人验收请求".into(),
                ));
            }
            let basis = d
                .acceptance
                .as_ref()
                .ok_or_else(|| Error::Conflict("验收请求缺少版本依据".into()))?;
            evidence(db, &task, basis)?;
            idle(db, &task, None)?;
            text(reason, "人类验收决定依据", 65536)?;
            if let Some(reference) = decision_ref {
                text(reference, "脱敏人类决定引用", 1024)?;
            }
            let record = AcceptanceDecision {
                id: d.id.clone(),
                task_id: task.id.clone(),
                task_revision: task.revision,
                artifact_id: basis.artifact_id.clone(),
                verification_id: basis.verification_id.clone(),
                actor: actor.into(),
                accepted: *accept,
                reason: reason.into(),
                decision_ref: decision_ref.clone(),
            };
            db.execute(
                "INSERT INTO acceptance_decisions(id,task_id,data) VALUES(?1,?2,?3)",
                params![record.id, task.id, serde_json::to_string(&record)?],
            )?;
            d.state = if *accept { "accepted" } else { "rejected" }.into();
            d.answer = Some(if *accept { "accept" } else { "reject" }.into());
            d.reason = Some(reason.into());
            d.revision += 1;
            db.execute("UPDATE deliveries SET status='handled',reason=?2,revision=revision+1 WHERE id=?1 AND run_id IS NULL",params![d.request_delivery,reason])?;
            if *accept {
                save_request(db, &d)?;
                task.state = "closed".into();
                task.outcome = Some("accepted".into());
                task.revision += 1;
                crate::decisions::supersede(db, &task.id, None, task.revision, "任务已验收关闭")?;
                crate::blocker::supersede(db, &task.id, "任务已验收关闭")?;
                db.execute("UPDATE deliveries SET status='cancelled',reason='任务已验收关闭',revision=revision+1 WHERE status IN ('queued','blocked') AND message_id IN (SELECT id FROM messages WHERE task_id=?1)",[&task.id])?;
                save_task(db, &task)?;
            } else {
                let leader = task.team_snapshot.leader.clone();
                let delivery=enqueue_system(db,&mut task,cause,Message {recipient:&leader,kind:"decision.result",body:&json!({"decisionId":d.id,"kind":"acceptance","state":"rejected","artifactId":record.artifact_id,"reason":reason,"next":"由团队负责人决定返工新版并独立检验；不得重新验收同一产出"}).to_string(),reply_to:None,event:Some(&format!("acceptance-rejected:{}:{leader}",d.id)),mandatory:true})?;
                d.response_delivery = Some(delivery["deliveryId"].as_str().unwrap().into());
                save_request(db, &d)?;
            }
            Ok(json!({"decision":d,"acceptance":record,"task":task}))
        }
        _ => Err(Error::Invalid("不是人工验收操作".into())),
    }
}
pub(crate) fn supersede(db: &Connection, task: &str, reason: &str) -> Result<()> {
    let mut stmt=db.prepare("SELECT data FROM decisions WHERE task_id=?1 AND json_extract(data,'$.kind')='acceptance' AND json_extract(data,'$.state')='open'")?;
    let rows = stmt
        .query_map([task], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for data in rows {
        let mut d: DecisionRequest = serde_json::from_str(&data)?;
        d.state = "superseded".into();
        d.reason = Some(reason.into());
        d.revision += 1;
        save_request(db, &d)?;
        db.execute("UPDATE deliveries SET status='cancelled',reason=?2,revision=revision+1 WHERE id=?1 AND status IN ('queued','blocked')",params![d.request_delivery,reason])?;
    }
    Ok(())
}
impl Store {
    pub fn acceptance_decision(&self, id: &str) -> Result<AcceptanceDecision> {
        load(&self.connection, "acceptance_decisions", id)
    }
}
