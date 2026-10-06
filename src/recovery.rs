//! Human recovery decisions for an unavailable digital team leader.
use crate::{
    Error, Result,
    model::*,
    store::{Message, enqueue_system, load, new_id, require, revision},
};
use rusqlite::{Connection, OptionalExtension, params};
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
    let body=json!({"decisionId":id,"kind":"recovery","deliveryId":delivery,"runId":run_id,"reason":reason,"options":["retry","wait","cancel"],"next":"用 task decision show 查看停止事实、当前产出和 retry/wait/cancel 各自的改变。正式回应是 task decision respond；回应尚未落实"}).to_string();
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
        impact: recovery_facts(db, &task.id, delivery)?["impact"]
            .as_str()
            .ok_or_else(|| Error::Conflict("恢复说明缺失".into()))?
            .into(),
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
                "本人选择 retry，同一条消息重新排队。不表示配置已修复，也不表示工作已经继续",
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

pub(crate) fn project(db: &Connection, mut value: Value) -> Result<Value> {
    if value.get("kind").and_then(|item| item.as_str()) != Some("recovery") {
        return Ok(value);
    }
    let delivery_id = value
        .pointer("/recovery/delivery_id")
        .and_then(|item| item.as_str())
        .ok_or_else(|| Error::Conflict("恢复事项缺少投递依据".into()))?
        .to_string();
    let task_id = value
        .get("task_id")
        .and_then(|item| item.as_str())
        .ok_or_else(|| Error::Conflict("恢复事项缺少任务".into()))?
        .to_string();
    let mut situation = recovery_facts(db, &task_id, &delivery_id)?;
    situation["response"] = json!(response_code(&value)?);
    value["impact"] = situation["impact"].clone();
    value["situation"] = situation;
    Ok(value)
}

fn response_code(value: &Value) -> Result<String> {
    let state = value
        .get("state")
        .and_then(|item| item.as_str())
        .ok_or_else(|| Error::Conflict("恢复事项缺少状态".into()))?;
    Ok(match state {
        "open" => "not_responded".to_string(),
        "responded"
            if value
                .get("blocked_reason")
                .and_then(|item| item.as_str())
                .is_some() =>
        {
            "apply_blocked".to_string()
        }
        "responded" => "responded_not_applied".to_string(),
        "resolved" => "applied".to_string(),
        other => other.to_string(),
    })
}

fn recovery_facts(db: &Connection, task_id: &str, delivery_id: &str) -> Result<Value> {
    let (receiver, status, reason, run_id, message_id): (
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    ) = db
        .query_row(
            "SELECT receiver,status,reason,run_id,message_id FROM deliveries WHERE id=?1",
            [delivery_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Error::Conflict("恢复投递不存在".into()),
            other => Error::Database(other),
        })?;
    let detail = reason.ok_or_else(|| Error::Conflict("恢复投递缺少停止事实".into()))?;
    let message_exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM messages WHERE id=?1)",
        [&message_id],
        |row| row.get(0),
    )?;
    if !message_exists {
        return Err(Error::Conflict("恢复消息不存在".into()));
    }
    let mut texts = vec![detail.clone()];
    if let Some(run_id) = &run_id {
        let run: Run = load(db, "runs", run_id)?;
        if run.worker_id != receiver {
            return Err(Error::Conflict("恢复投递的成员与 Run 成员不一致".into()));
        }
        if let Some(stop_reason) = run.stop_reason {
            texts.push(stop_reason);
        }
        terminal_texts(db, run_id, &mut texts)?;
    }
    let has_disposition: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM message_dispositions WHERE delivery_id=?1)",
        [delivery_id],
        |row| row.get(0),
    )?;
    let fact = classify(&texts, has_disposition);
    let task: Task = load(db, "tasks", task_id)?;
    let (artifact_id, artifact_partial, differs_from_baseline) = artifact_basis(db, &task)?;
    Ok(json!({
        "member_id": receiver,
        "message_id": message_id,
        "run_id": run_id,
        "delivery_id": delivery_id,
        "delivery_status": status,
        "stop_fact": fact,
        "stop_detail": detail,
        "impact": impact_for(fact, &detail),
        "artifact_id": artifact_id,
        "artifact_partial": artifact_partial,
        "differs_from_baseline": differs_from_baseline,
        "choices": choices(),
    }))
}

fn artifact_basis(
    db: &Connection,
    task: &Task,
) -> Result<(Option<String>, Option<bool>, Option<bool>)> {
    let Some(artifact_id) = task.current_artifact.clone() else {
        return Ok((None, None, None));
    };
    let artifact: crate::artifact::Artifact = load(db, "artifacts", &artifact_id)?;
    let differs = if let Some(input_id) = &task.contract.code_input {
        let input: crate::content::GitInput = load(db, "inputs", input_id)?;
        Some(files_differ(&artifact.files, &input.files))
    } else {
        None
    };
    Ok((Some(artifact_id), Some(artifact.partial), differs))
}

fn files_differ(left: &[crate::content::FileEntry], right: &[crate::content::FileEntry]) -> bool {
    use std::collections::BTreeMap;
    fn index(files: &[crate::content::FileEntry]) -> Option<BTreeMap<&str, (&str, u64, bool)>> {
        let mut map = BTreeMap::new();
        for file in files {
            if map
                .insert(
                    file.path.as_str(),
                    (file.sha256.as_str(), file.size, file.executable),
                )
                .is_some()
            {
                return None;
            }
        }
        Some(map)
    }
    match (index(left), index(right)) {
        (Some(left), Some(right)) => left != right,
        _ => true,
    }
}

fn terminal_texts(db: &Connection, run_id: &str, texts: &mut Vec<String>) -> Result<()> {
    for sql in [
        "SELECT json_extract(data,'$.terminal') FROM api_launches WHERE run_id=?1",
        "SELECT json_extract(data,'$.terminal') FROM cli_resources WHERE run_id=?1",
    ] {
        let raw: Option<Option<String>> = db
            .query_row(sql, [run_id], |row| row.get::<_, Option<String>>(0))
            .optional()?;
        if let Some(Some(raw)) = raw {
            push_terminal(&raw, texts)?;
        }
    }
    Ok(())
}

fn push_terminal(raw: &str, texts: &mut Vec<String>) -> Result<()> {
    if raw == "null" {
        return Ok(());
    }
    let parsed: Value =
        serde_json::from_str(raw).map_err(|_| Error::Conflict("恢复停止终态无法读取".into()))?;
    if parsed.is_null() {
        return Ok(());
    }
    let object = parsed
        .as_object()
        .ok_or_else(|| Error::Conflict("恢复停止终态无法读取".into()))?;
    for key in [
        "stopReason",
        "stop_reason",
        "stopCode",
        "stop_code",
        "nativeStopReason",
        "native_stop_reason",
    ] {
        if let Some(text) = object.get(key).and_then(|item| item.as_str()) {
            texts.push(text.to_string());
        }
    }
    Ok(())
}

fn classify(texts: &[String], has_disposition: bool) -> &'static str {
    let joined = texts.join("\n");
    if joined.contains("权限已撤销") {
        return "permission";
    }
    if joined.contains("budget_exhausted")
        || joined.contains("额度耗尽")
        || joined.contains("额度已耗尽")
    {
        return "quota";
    }
    if joined.contains("MODEL_CONNECTION_ERROR") {
        return "connection_failure";
    }
    if !has_disposition && joined.contains("model_stop") {
        return "model_stop_without_message_respond";
    }
    "observed"
}

fn impact_for(fact: &str, detail: &str) -> String {
    let mut label = match fact {
        "model_stop_without_message_respond" => {
            "模型结束本轮，但没有 message_respond。这不是修复配置。".to_string()
        }
        "connection_failure" => "连接失败：提供方请求没有连上。这不是修复配置。".to_string(),
        "permission" => "权限不足或已撤销。可以恢复原授权，或等待、取消。".to_string(),
        "quota" => "额度不足。retry 不重置额度。".to_string(),
        _ => {
            let mut text = format!("停止事实：{detail}");
            if detail.contains("配置") || detail.contains("凭据") || detail.contains("登录") {
                text.push_str(" 可先修复原配置范围内的该项，再选择 retry。");
            }
            text
        }
    };
    label.push_str(" retry 把同一条消息重新排队给原团队负责人，不表示工作已经继续。");
    label
}

fn choices() -> Value {
    json!({
        "retry": "把同一条消息重新排队给原团队负责人。不表示工作已经继续，不授予权限，不解除未知资源，不重置额度。",
        "wait": "保留本待决定事项。不重新排队，不取消任务。",
        "cancel": "提交任务取消，并等待资源停止后才关闭。不把仍有活动资源的任务立刻标成已结束。"
    })
}

#[cfg(test)]
mod tests {
    use super::files_differ;
    use crate::content::FileEntry;

    fn entry(path: &str, sha: &str) -> FileEntry {
        FileEntry {
            path: path.into(),
            sha256: sha.into(),
            size: 1,
            executable: false,
        }
    }

    #[test]
    fn file_difference_ignores_order_and_rejects_duplicate_paths() {
        let left = vec![entry("a", "1"), entry("b", "2")];
        let right = vec![entry("b", "2"), entry("a", "1")];
        assert!(!files_differ(&left, &right));
        let changed = vec![entry("a", "9"), entry("b", "2")];
        assert!(files_differ(&left, &changed));
        let duplicated = vec![entry("a", "1"), entry("a", "1")];
        assert!(files_differ(&left, &duplicated));
    }
}
