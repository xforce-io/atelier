//! Formal clarification decisions. Responding records intent; only existing
//! business operations can prove that a requested change actually happened.
use crate::{
    Error, Result,
    model::*,
    store::{Message, enqueue, load, new_id, participants, revision, text},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

fn save(db: &Connection, decision: &DecisionRequest) -> Result<()> {
    db.execute("INSERT INTO decisions(id,task_id,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data", params![decision.id,decision.task_id,serde_json::to_string(decision)?])?;
    Ok(())
}

fn current(db: &Connection, decision: &DecisionRequest) -> Result<Task> {
    let task: Task = load(db, "tasks", &decision.task_id)?;
    if task.state == "closed"
        || task.cancellation_requested
        || decision.effective_revision != task.revision
    {
        return Err(Error::Conflict(format!(
            "决定依据已过期，当前任务版本为 {}",
            task.revision
        )));
    }
    Ok(task)
}

pub(crate) fn bind_change(
    db: &Connection,
    id: &str,
    actor: &str,
    cause: &str,
    task_id: Option<&String>,
    team_id: Option<&String>,
) -> Result<()> {
    let mut decision: DecisionRequest = load(db, "decisions", id)?;
    let task = current(db, &decision)?;
    if decision.kind != "clarification"
        || decision.state != "responded"
        || decision.operation.is_some()
    {
        return Err(Error::Conflict("决定必须已回应且尚无关联变更".into()));
    }
    if actor != decision.requester && actor != decision.handler {
        return Err(Error::Forbidden("只有决定参与者可关联落实操作".into()));
    }
    if task_id.is_some_and(|id| *id != task.id) || team_id.is_some_and(|id| *id != task.team_id) {
        return Err(Error::Invalid("不能关联其他任务或团队的决定".into()));
    }
    decision.operation = Some(OperationReference {
        actor: actor.into(),
        request_id: cause.into(),
    });
    decision.revision += 1;
    decision.reason = None;
    decision.blocked_reason = None;
    save(db, &decision)
}

pub(crate) fn supersede(
    db: &Connection,
    task_id: &str,
    keep: Option<&str>,
    new_revision: u64,
    reason: &str,
) -> Result<()> {
    let mut stmt = db.prepare("SELECT data FROM decisions WHERE task_id=?1")?;
    let rows = stmt
        .query_map([task_id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for row in rows {
        let mut decision: DecisionRequest = serde_json::from_str(&row)?;
        if !matches!(decision.state.as_str(), "open" | "responded") {
            continue;
        }
        if keep == Some(decision.id.as_str()) {
            decision.effective_revision = new_revision;
        } else {
            decision.state = "superseded".into();
            decision.reason = Some(reason.into());
            decision.revision += 1;
            for delivery in [
                Some(&decision.request_delivery),
                decision.response_delivery.as_ref(),
            ]
            .into_iter()
            .flatten()
            {
                db.execute("UPDATE deliveries SET status='cancelled',reason=?2,revision=revision+1 WHERE id=?1 AND status IN ('queued','blocked')",params![delivery,reason])?;
            }
        }
        save(db, &decision)?;
    }
    Ok(())
}

pub(crate) fn apply(db: &Connection, actor: &str, cause: &str, command: &Command) -> Result<Value> {
    match command {
        Command::DecisionRequest {
            task_id,
            revision: expected,
            handler,
            question,
            impact,
            options,
        } => {
            let mut task: Task = load(db, "tasks", task_id)?;
            revision(task.revision, *expected)?;
            if task.state == "closed" {
                return Err(Error::Conflict("任务已关闭".into()));
            }
            if !participants(&task).contains(&actor)
                || !participants(&task).contains(&handler.as_str())
            {
                return Err(Error::Forbidden("决定仅限本任务参与者".into()));
            }
            text(question, "决定问题", 65536)?;
            text(impact, "决定影响", 65536)?;
            if options.len() > 16 {
                return Err(Error::Invalid("最多提供 16 个选项".into()));
            }
            for (index, option) in options.iter().enumerate() {
                text(option, "选项", 1024)?;
                if options[..index].contains(option) {
                    return Err(Error::Invalid("选项不能重复".into()));
                }
            }
            let id = new_id();
            let body = serde_json::to_string(
                &json!({"decisionId":id,"question":question,"impact":impact,"options":options,"input":if options.is_empty(){"text"}else{"choice"}}),
            )?;
            let delivery = enqueue(
                db,
                &mut task,
                actor,
                cause,
                Message {
                    recipient: handler,
                    kind: "decision.request",
                    body: &body,
                    reply_to: None,
                    event: Some(&format!("decision:{id}:request")),
                    mandatory: false,
                },
            )?;
            let decision = DecisionRequest {
                recovery: None,
                acceptance: None,
                id,
                task_id: task_id.clone(),
                task_revision: task.revision,
                effective_revision: task.revision,
                requester: actor.into(),
                handler: handler.clone(),
                kind: "clarification".into(),
                question: question.clone(),
                impact: impact.clone(),
                options: options.clone(),
                revision: 1,
                state: "open".into(),
                answer: None,
                blocked_reason: None,
                reason: None,
                operation: None,
                request_delivery: delivery["deliveryId"]
                    .as_str()
                    .ok_or_else(|| Error::Invalid("决定投递缺失".into()))?
                    .into(),
                response_delivery: None,
            };
            save(db, &decision)?;
            Ok(json!({"decision":decision,"delivery":delivery}))
        }
        Command::DecisionRespond {
            id,
            revision: expected,
            answer,
        } => {
            let mut decision: DecisionRequest = load(db, "decisions", id)?;
            if decision.kind == "recovery" {
                return crate::recovery::respond(db, actor, id, *expected, answer);
            }
            let mut task = current(db, &decision)?;
            revision(decision.revision, *expected)?;
            if decision.kind != "clarification" || decision.state != "open" {
                return Err(Error::Conflict("该事项不能通过普通决定回应".into()));
            }
            text(answer, "正式回应", 65536)?;
            if !decision.options.is_empty() && !decision.options.contains(answer) {
                return Err(Error::Invalid("回应不属于允许的选项".into()));
            }
            decision.answer = Some(answer.clone());
            decision.state = "responded".into();
            decision.revision += 1;
            let body = serde_json::to_string(
                &json!({"decisionId":id,"answer":answer,"state":"responded","next":"由发起者落实或记录落实受阻，不自动修改业务对象"}),
            )?;
            let delivery = enqueue(
                db,
                &mut task,
                actor,
                cause,
                Message {
                    recipient: &decision.requester,
                    kind: "decision.result",
                    body: &body,
                    reply_to: None,
                    event: Some(&format!("decision:{id}:response")),
                    mandatory: false,
                },
            )?;
            decision.response_delivery = Some(
                delivery["deliveryId"]
                    .as_str()
                    .ok_or_else(|| Error::Invalid("决定结果投递缺失".into()))?
                    .into(),
            );
            // Human receipts have no Run. Digital receipts need a separately
            // saved disposition and observed resource stop before handled.
            if load::<Worker>(db, "workers", actor)?.kind == WorkerKind::Human {
                db.execute("UPDATE deliveries SET status='handled',reason='正式回应已保存',revision=revision+1 WHERE id=?1 AND run_id IS NULL",[&decision.request_delivery])?;
            }
            save(db, &decision)?;
            Ok(json!({"decision":decision,"delivery":delivery}))
        }
        Command::DecisionRecord {
            id,
            revision: expected,
            resolution,
        } => {
            let mut decision: DecisionRequest = load(db, "decisions", id)?;
            let task = current(db, &decision)?;
            revision(decision.revision, *expected)?;
            if decision.kind != "clarification" || decision.state != "responded" {
                return Err(Error::Conflict("只能落实已回应的普通决定".into()));
            }
            match resolution {
                DecisionResolution::Operation { reference } => {
                    if decision.operation.as_ref() != Some(reference) {
                        return Err(Error::Invalid(
                            "必须引用显式关联此决定的已提交业务操作".into(),
                        ));
                    }
                    let record: Option<(String, String)> = db
                        .query_row(
                            "SELECT command,result FROM requests WHERE actor=?1 AND id=?2",
                            params![reference.actor, reference.request_id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    let (command, result) =
                        record.ok_or_else(|| Error::NotFound("关联业务操作尚未提交".into()))?;
                    let result: Value = serde_json::from_str(&result)?;
                    match serde_json::from_str::<Command>(&command)? {
                        Command::TaskUpdate {
                            decision_id: Some(basis),
                            id: task_id,
                            ..
                        } if basis == decision.id
                            && task_id == task.id
                            && result["task"]["revision"] == task.revision => {}
                        Command::PermissionsUpdate {
                            decision_id: Some(basis),
                            team_id,
                            ..
                        } if basis == decision.id && team_id == task.team_id => {
                            let team: Team = load(db, "teams", &team_id)?;
                            if result["team"]["authorization_revision"]
                                != team.authorization_revision
                            {
                                return Err(Error::Conflict("关联授权结果已被后续变更取代".into()));
                            }
                        }
                        _ => {
                            return Err(Error::Invalid(
                                "关联操作不是该决定当前有效的落实依据".into(),
                            ));
                        }
                    }
                    decision.state = "resolved".into();
                    decision.reason = Some("已核对关联业务操作".into());
                    decision.blocked_reason = None;
                }
                DecisionResolution::NoChange { reason } => {
                    text(reason, "无需业务变更的原因", 65536)?;
                    if decision.operation.is_some() {
                        return Err(Error::Conflict("已有业务变更，须引用该操作核对".into()));
                    }
                    decision.state = "resolved".into();
                    decision.reason = Some(reason.clone());
                    decision.blocked_reason = None;
                }
                DecisionResolution::Blocked { reason } => {
                    text(reason, "落实受阻原因", 65536)?;
                    decision.blocked_reason = Some(reason.clone());
                }
            }
            decision.revision += 1;
            if decision.state == "resolved"
                && load::<Worker>(db, "workers", actor)?.kind == WorkerKind::Human
            {
                if let Some(delivery) = &decision.response_delivery {
                    db.execute("UPDATE deliveries SET status='handled',reason=?2,revision=revision+1 WHERE id=?1 AND run_id IS NULL",params![delivery,decision.reason])?;
                }
            }
            save(db, &decision)?;
            Ok(json!(decision))
        }
        _ => Err(Error::Invalid("不是待决定事项操作".into())),
    }
}
