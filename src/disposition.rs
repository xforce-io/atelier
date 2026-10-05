//! A durable handling result is a separate fact from process termination.
use crate::{
    Error, Result,
    model::{DecisionRequest, Run, Task},
    store::{Message, enqueue, load, participants, text},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Disposition {
    Wait { reason: String, handler: String },
    Reply { message_id: String },
    Decision { decision_id: String },
    Assignment { message_id: String },
    Retry { delivery_id: String },
}

pub(crate) fn exists(db: &Connection, delivery: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM message_dispositions WHERE delivery_id=?1)",
        [delivery],
        |r| r.get(0),
    )?)
}

pub(crate) fn save(
    db: &Connection,
    run: &Run,
    task: &Task,
    disposition: &Disposition,
) -> Result<Value> {
    if run.purpose != "coordinate" {
        return Err(Error::Forbidden(
            "执行或检验投递须保存对应业务终局，不能用协调回应结束".into(),
        ));
    }
    if exists(db, &run.delivery_id)? {
        return Err(Error::Conflict("该投递已有持久处理结果".into()));
    }
    let (message_id,kind,sender):(String,String,Option<String>)=db.query_row(
        "SELECT m.id,m.kind,m.sender FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE d.id=?1",[&run.delivery_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    match disposition {
        Disposition::Retry { delivery_id } => {
            let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM member_requests r JOIN deliveries d ON d.id=?2 JOIN messages m ON m.id=d.message_id WHERE r.delivery_id=?1 AND json_extract(r.result,'$.ok')=1 AND json_extract(r.result,'$.data.operation')='mailbox_retry' AND json_extract(r.result,'$.data.delivery.deliveryId')=d.id AND json_extract(r.result,'$.data.delivery.status')='queued' AND d.status='queued' AND m.task_id=?3)",params![run.delivery_id,delivery_id,task.id],|r|r.get(0))?;
            if !valid
                || task.team_snapshot.leader != run.worker_id
                || matches!(kind.as_str(), "decision.request" | "decision.result")
            {
                return Err(Error::Conflict(
                    "须引用本投递期间实际提交且仍待处理的同任务重试；正式决定另行落实".into(),
                ));
            }
        }

        Disposition::Assignment {
            message_id: assignment,
        } => {
            let rejected_acceptance: Option<String> = if kind == "decision.result" {
                db.query_row("SELECT id FROM decisions WHERE task_id=?1 AND json_extract(data,'$.response_delivery')=?2 AND json_extract(data,'$.kind')='acceptance' AND json_extract(data,'$.state')='rejected'",params![task.id,run.delivery_id],|r|r.get(0)).optional()?
            } else {
                None
            };
            if kind == "decision.request"
                || (kind == "decision.result" && rejected_acceptance.is_none())
            {
                return Err(Error::Conflict(
                    "正式决定投递须回应或核对对应决定，不能仅以执行安排结束".into(),
                ));
            }
            if let Some(rejected) = rejected_acceptance {
                let matches:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND kind='assignment.rework' AND json_extract(body,'$.reason.kind')='rejection' AND json_extract(body,'$.reason.id')=?2)",params![assignment,rejected],|r|r.get(0))?;
                if !matches {
                    return Err(Error::Conflict(
                        "须引用基于本次人类拒绝实际提交的返工安排".into(),
                    ));
                }
            }
            let valid: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM messages m JOIN deliveries d ON d.message_id=m.id JOIN member_requests r ON r.operation_id=m.causation_id WHERE m.id=?1 AND m.task_id=?2 AND m.task_revision=?3 AND m.sender=?4 AND m.kind IN ('assignment.execute','assignment.rework','handoff.verify') AND m.source='core' AND d.status IN ('queued','claimed','handled') AND r.delivery_id=?5 AND json_extract(r.result,'$.ok')=1 AND json_extract(r.result,'$.data.delivery.messageId')=m.id)", params![assignment,task.id,task.revision,run.worker_id,run.delivery_id], |r| r.get(0))?;
            if task.state != "active"
                || (task.team_snapshot.leader != run.worker_id
                    && task.team_snapshot.executor.as_deref() != Some(run.worker_id.as_str()))
                || !valid
            {
                return Err(Error::Invalid(
                    "必须引用本投递处理期间已提交且有效的执行安排或交接；受阻安排须明确等待处理者"
                        .into(),
                ));
            }
        }
        Disposition::Wait { reason, handler } => {
            text(reason, "等待原因", 65536)?;
            if !participants(task).contains(&handler.as_str()) {
                return Err(Error::Forbidden("等待责任人必须参与本任务".into()));
            }
            if handler != &run.worker_id {
                let mut updated = task.clone();
                let body=json!({"deliveryId":run.delivery_id,"runId":run.id,"state":"waiting","reason":reason,"handler":handler,"next":"按当前任务职责补充资料或回复；此通知不自动改变任务状态"}).to_string();
                enqueue(
                    db,
                    &mut updated,
                    &run.worker_id,
                    &run.id,
                    Message {
                        recipient: handler,
                        kind: "result",
                        body: &body,
                        reply_to: Some(&message_id),
                        event: Some(&format!("waiting:{}:{handler}", run.delivery_id)),
                        mandatory: false,
                    },
                )?;
            }
        }
        Disposition::Reply { message_id: reply } => {
            if matches!(
                kind.as_str(),
                "intake" | "intake.updated" | "decision.request" | "decision.result"
            ) {
                return Err(Error::Conflict(
                    "正式承接或决定投递不能以普通回复代替业务决定".into(),
                ));
            }
            let recipient = sender.as_deref().unwrap_or(&task.team_snapshot.acceptor);
            let valid:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1 AND task_id=?2 AND sender=?3 AND recipient=?4 AND reply_to=?5 AND kind IN ('work.note','work.question'))",
                params![reply,task.id,run.worker_id,recipient,message_id],|r|r.get(0))?;
            if !valid {
                return Err(Error::Invalid(
                    "必须引用当前成员对原消息发送者的已提交回复".into(),
                ));
            }
        }
        Disposition::Decision { decision_id } => {
            let decision: DecisionRequest = load(db, "decisions", decision_id)?;
            if decision.task_id != task.id
                || decision.effective_revision != task.revision
                || decision.requester != run.worker_id
                || !matches!(decision.state.as_str(), "open" | "responded" | "resolved")
            {
                return Err(Error::Invalid(
                    "必须引用当前成员在同一任务版本下的有效待决定事项".into(),
                ));
            }
            if kind == "decision.request" {
                return Err(Error::Conflict(
                    "收到的正式决定请求须保存正式回应或等待依据".into(),
                ));
            }
            if kind == "decision.result"
                && decision.response_delivery.as_deref() != Some(run.delivery_id.as_str())
            {
                return Err(Error::Invalid(
                    "决定结果须引用该通知对应的待决定事项".into(),
                ));
            }
            if kind == "decision.result" && decision.state != "resolved" {
                return Err(Error::Conflict(
                    "决定回应尚未落实；受阻时须保存带责任人的等待结果".into(),
                ));
            }
        }
    }
    record(db, run, task, serde_json::to_value(disposition)?)
}

pub(crate) fn record(db: &Connection, run: &Run, task: &Task, outcome: Value) -> Result<Value> {
    let result = json!({"deliveryId":run.delivery_id,"runId":run.id,"taskRevision":task.revision,"workerId":run.worker_id,"result":outcome,"state":"recorded","next":"结束本轮；核心确认资源停止后核对投递终局"});
    db.execute("INSERT INTO message_dispositions(delivery_id,run_id,task_revision,data) VALUES(?1,?2,?3,?4)",params![run.delivery_id,run.id,task.revision,serde_json::to_string(&result)?])?;
    Ok(result)
}

pub(crate) fn valid_for_stop(db: &Connection, run: &Run, task: &Task) -> Result<bool> {
    let saved: Option<(String, u64)> = db
        .query_row(
            "SELECT run_id,task_revision FROM message_dispositions WHERE delivery_id=?1",
            [&run.delivery_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(saved.is_some_and(|(id, revision)| id == run.id && revision == task.revision))
}
