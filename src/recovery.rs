//! Human recovery decisions for an unavailable digital team leader.
use crate::{
    Error, Result,
    model::*,
    store::{Message, enqueue_system, load, new_id, require, revision},
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryBasis {
    pub delivery_id: String,
    pub event: String,
}
fn save(db: &Connection, d: &DecisionRequest) -> Result<()> {
    crate::acceptance::save_request(db, d)
}
pub(crate) fn authorize(db: &Connection, actor: &str, id: &str) -> Result<(DecisionRequest, Task)> {
    let d: DecisionRequest = load(db, "decisions", id)?;
    let task: Task = load(db, "tasks", &d.task_id)?;
    if d.kind != "recovery"
        || d.handler != actor
        || task.team_snapshot.acceptor != actor
        || load::<Worker>(db, "workers", actor)?.kind != WorkerKind::Human
    {
        return Err(Error::Forbidden("恢复事项只能由本任务本人处理".into()));
    }
    let team: Team = load(db, "teams", &task.team_id)?;
    for p in [Permission::Manage, Permission::Communicate] {
        require(&team, actor, p.clone())?;
        require(&task.team_snapshot, actor, p)?;
    }
    Ok((d, task))
}
fn current(d: &DecisionRequest, t: &Task) -> Result<()> {
    if t.state == "closed" || t.cancellation_requested || t.revision != d.effective_revision {
        return Err(Error::Conflict("恢复依据已过期或任务已取消/关闭".into()));
    }
    Ok(())
}
pub(crate) fn ensure(
    db: &Connection,
    task: &mut Task,
    delivery: &str,
    event: &str,
    reason: &str,
) -> Result<bool> {
    let (receiver, status, run_id): (String, String, Option<String>) = db.query_row(
        "SELECT receiver,status,run_id FROM deliveries WHERE id=?1",
        [delivery],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if task.state == "closed"
        || task.cancellation_requested
        || receiver != task.team_snapshot.leader
        || !task
            .worker_snapshots
            .get(&receiver)
            .is_some_and(|w| w.kind == WorkerKind::Agent)
        || !matches!(status.as_str(), "blocked" | "uncertain")
    {
        return Ok(false);
    }
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE task_id=?1 AND json_extract(data,'$.kind')='recovery' AND json_extract(data,'$.recovery.delivery_id')=?2 AND json_extract(data,'$.recovery.event')=?3)",params![task.id,delivery,event],|r|r.get(0))?;
    if exists {
        return Ok(true);
    }
    let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE task_id=?1 AND json_extract(data,'$.kind')='recovery' AND json_extract(data,'$.recovery.delivery_id')=?2 AND json_extract(data,'$.state') IN ('open','responded'))",params![task.id,delivery],|r|r.get(0))?;
    if pending {
        return Ok(true);
    }

    let id = new_id();
    let human = task.team_snapshot.acceptor.clone();
    let body=json!({"decisionId":id,"kind":"recovery","deliveryId":delivery,"runId":run_id,"reason":reason,"options":["retry","wait","cancel"],"next":"先正式回应；retry 不授予权限或解除未知，修复后显式落实；wait 保留待办，cancel 另提交取消"}).to_string();
    let receipt = enqueue_system(
        db,
        task,
        event,
        Message {
            recipient: &human,
            kind: "decision.request",
            body: &body,
            reply_to: None,
            event: Some(&format!("recovery:{id}")),
            mandatory: true,
        },
    )?;
    let d = DecisionRequest {
        acceptance: None,
        recovery: Some(RecoveryBasis {
            delivery_id: delivery.into(),
            event: event.into(),
        }),
        id,
        task_id: task.id.clone(),
        task_revision: task.revision,
        effective_revision: task.revision,
        requester: human.clone(),
        handler: human,
        kind: "recovery".into(),
        question: reason.into(),
        impact: "本人修复原配置/授权和核对资源；业务承接与安排仍由原团队负责人决定".into(),
        options: vec!["retry".into(), "wait".into(), "cancel".into()],
        revision: 1,
        state: "open".into(),
        answer: None,
        blocked_reason: None,
        reason: None,
        operation: None,
        request_delivery: receipt["deliveryId"].as_str().unwrap().into(),
        response_delivery: None,
    };
    save(db, &d)?;
    Ok(true)
}
pub(crate) fn respond(
    db: &Connection,
    actor: &str,
    id: &str,
    expected: u64,
    answer: &str,
) -> Result<Value> {
    let (mut d, mut task) = authorize(db, actor, id)?;
    current(&d, &task)?;
    revision(d.revision, expected)?;
    if !matches!(d.state.as_str(), "open" | "responded") || !d.options.iter().any(|a| a == answer) {
        return Err(Error::Conflict(
            "恢复事项只能选择 retry/wait/cancel，终态不能重写".into(),
        ));
    }
    if let Some(prior) = &d.response_delivery {
        db.execute("UPDATE deliveries SET status='cancelled',reason='本人更新了恢复选择',revision=revision+1 WHERE id=?1 AND status IN ('queued','blocked')",[prior])?;
    }
    d.answer = Some(answer.into());
    d.state = "responded".into();
    d.revision += 1;
    d.blocked_reason = None;
    d.reason = Some(
        if answer == "wait" {
            "本人选择继续等待，可在同事项更新选择"
        } else {
            "本人选择已保存，尚未落实恢复操作"
        }
        .into(),
    );
    db.execute("UPDATE deliveries SET status='handled',reason='本人已正式回应恢复事项',revision=revision+1 WHERE id=?1 AND run_id IS NULL",[&d.request_delivery])?;
    let body=json!({"decisionId":d.id,"kind":"recovery","answer":answer,"state":"responded","next":"恢复条件就绪后使用 task recovery apply；当前回应没有重新排队或取消任务"}).to_string();
    let result = enqueue_system(
        db,
        &mut task,
        &d.id,
        Message {
            recipient: actor,
            kind: "decision.result",
            body: &body,
            reply_to: None,
            event: Some(&format!("recovery-response:{}:{}", d.id, d.revision)),
            mandatory: true,
        },
    )?;
    d.response_delivery = Some(result["deliveryId"].as_str().unwrap().into());
    save(db, &d)?;
    Ok(json!({"decision":d,"delivery":result}))
}
pub(crate) fn require_retry_choice(db: &Connection, delivery: &str) -> Result<()> {
    let blocks:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM decisions WHERE json_extract(data,'$.kind')='recovery' AND json_extract(data,'$.recovery.delivery_id')=?1 AND json_extract(data,'$.state') IN ('open','responded') AND coalesce(json_extract(data,'$.answer'),'')!='retry')",[delivery],|r|r.get(0))?;
    if blocks {
        return Err(Error::Conflict(
            "此投递有本人恢复待办；须先正式选择 retry，等待或取消选择不能重新排队".into(),
        ));
    }
    Ok(())
}
pub(crate) fn resolve_retried(
    db: &Connection,
    actor: &str,
    delivery: &str,
    cause: &str,
) -> Result<()> {
    let mut stmt=db.prepare("SELECT data FROM decisions WHERE json_extract(data,'$.kind')='recovery' AND json_extract(data,'$.recovery.delivery_id')=?1 AND json_extract(data,'$.state')='responded' AND json_extract(data,'$.answer')='retry'")?;
    let rows = stmt
        .query_map([delivery], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for row in rows {
        let mut d: DecisionRequest = serde_json::from_str(&row)?;
        d.state = "resolved".into();
        d.revision += 1;
        d.reason = Some("已核对投递结果或提交重新评估；queued 不代表环境或模型已成功".into());
        d.blocked_reason = None;
        d.operation = Some(OperationReference {
            actor: actor.into(),
            request_id: cause.into(),
        });
        finish(db, &d)?;
    }
    Ok(())
}
fn finish(db: &Connection, d: &DecisionRequest) -> Result<()> {
    if let Some(id) = &d.response_delivery {
        db.execute("UPDATE deliveries SET status='handled',reason=?2,revision=revision+1 WHERE id=?1 AND run_id IS NULL",params![id,d.reason])?;
    }
    save(db, d)
}
pub(crate) fn apply(
    db: &Connection,
    actor: &str,
    cause: &str,
    id: &str,
    expected: u64,
) -> Result<Value> {
    let (mut d, task) = authorize(db, actor, id)?;
    current(&d, &task)?;
    revision(d.revision, expected)?;
    if d.state != "responded" {
        return Err(Error::Conflict("先由本人正式回应恢复事项".into()));
    }
    if d.answer.as_deref() == Some("wait") {
        return Ok(json!({"decision":d,"applied":false,"next":"继续等待，未安排运行"}));
    }
    db.execute_batch("SAVEPOINT recovery_effect;")?;
    let effect = match d.answer.as_deref() {
        Some("retry") => {
            let delivery = &d
                .recovery
                .as_ref()
                .ok_or_else(|| Error::Conflict("恢复投递依据缺失".into()))?
                .delivery_id;
            let rev: u64 = db.query_row(
                "SELECT revision FROM deliveries WHERE id=?1",
                [delivery],
                |r| r.get(0),
            )?;
            crate::retry::apply(
                db,
                actor,
                cause,
                delivery,
                rev,
                "本人恢复决定：原配置条件已修复，请重新检查",
            )
        }
        Some("cancel") => crate::store::apply(
            db,
            actor,
            cause,
            &Command::TaskCancel {
                id: task.id.clone(),
                revision: task.revision,
                reason: format!("本人恢复事项 {} 决定取消", d.id),
            },
        ),
        _ => Err(Error::Conflict("恢复选择无效".into())),
    };
    match effect {
        Ok(result) => {
            db.execute_batch("RELEASE recovery_effect;")?;
            d = load(db, "decisions", id)?;
            if d.state != "resolved" {
                d.state = "resolved".into();
                d.revision += 1;
                d.reason = Some("恢复操作已提交；取消仍须等待资源停止".into());
                d.operation = Some(OperationReference {
                    actor: actor.into(),
                    request_id: cause.into(),
                });
                d.blocked_reason = None;
                finish(db, &d)?;
            }
            Ok(json!({"decision":d,"applied":true,"result":result}))
        }
        Err(
            error @ (Error::Database(_) | Error::Io(_) | Error::Unavailable(_) | Error::Json(_)),
        ) => Err(error),
        Err(error) => {
            db.execute_batch("ROLLBACK TO recovery_effect; RELEASE recovery_effect;")?;
            d.revision += 1;
            d.blocked_reason = Some(error.to_string());
            save(db, &d)?;
            Ok(
                json!({"decision":d,"applied":false,"error":{"code":error.code(),"message":error.to_string()}}),
            )
        }
    }
}
pub(crate) fn revoked(
    db: &Connection,
    team: &str,
    event: &str,
    revoke: &std::collections::BTreeMap<String, Vec<Permission>>,
) -> Result<()> {
    let mut s=db.prepare("SELECT d.id,m.task_id FROM deliveries d JOIN messages m ON m.id=d.message_id JOIN tasks t ON t.id=m.task_id WHERE t.team_id=?1 AND d.status='blocked' AND d.reason='成员所需权限已撤销'")?;
    let rows = s
        .query_map([team], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (id, task) in rows {
        let mut task: Task = load(db, "tasks", &task)?;
        if revoke.get(&task.team_snapshot.leader).is_some_and(|ps| {
            ps.iter()
                .any(|p| matches!(p, Permission::Arrange | Permission::Communicate))
        }) {
            ensure(db, &mut task, &id, event, "团队负责人所需权限已撤销")?;
        }
    }
    Ok(())
}
