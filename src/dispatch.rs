//! Mechanical discovery of existing mailbox deliveries, not a workflow planner.
use crate::{
    Result,
    model::{Task, WorkerKind},
    store::{Message, Store, enqueue_system, load},
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

#[derive(Clone)]
pub(crate) struct Work {
    pub delivery_id: String,
    pub revision: u64,
    pub message_revision: u64,
    pub task: Task,
    pub worker_id: String,
    pub configuration: Option<String>,
    pub purpose: String,
    pub message: Value,
}

impl Store {
    pub(crate) fn next_work(&self, epoch: &str) -> Result<Option<Work>> {
        let tx = self.connection.unchecked_transaction()?;
        crate::runs::service(&tx, epoch, false)?;
        let stopping: bool = tx.query_row(
            "SELECT stop_requested FROM runtime WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if stopping {
            return Ok(None);
        }
        if crate::runs::active_count(&tx)? != 0 {
            return Ok(None);
        }
        let mut statement = tx.prepare("SELECT d.id,d.revision,d.receiver,m.task_id,m.kind,m.id,m.sender,m.source,m.body,m.reply_to,m.causation_id,m.task_revision FROM deliveries d JOIN messages m ON m.id=d.message_id WHERE d.status='queued' ORDER BY m.rowid")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let task: Task = load(&tx, "tasks", &row.get::<_, String>(3)?)?;
            let receiver: String = row.get(2)?;
            let worker = match task.worker_snapshots.get(&receiver) {
                Some(worker) => worker.clone(),
                // A pending refresh may remove the old receiver. Keep reading
                // that historical delivery so the stale version can be closed.
                None => load(&tx, "workers", &receiver)?,
            };
            if worker.kind == WorkerKind::Human {
                continue;
            }
            let kind: String = row.get(4)?;
            let purpose = crate::runs::delivery_purpose(&kind);
            return Ok(Some(Work {
                delivery_id: row.get(0)?,
                revision: row.get(1)?,
                message_revision: crate::retry::effective_revision(
                    &tx,
                    &row.get::<_, String>(0)?,
                    row.get(11)?,
                )?,
                configuration: worker.execution_config.clone(),
                worker_id: receiver,
                purpose: purpose.into(),
                message: json!({"id":row.get::<_,String>(5)?,"sender":row.get::<_,Option<String>>(6)?,
                    "source":row.get::<_,String>(7)?,"kind":kind,"body":row.get::<_,String>(8)?,
                    "replyTo":row.get::<_,Option<String>>(9)?,"causationId":row.get::<_,String>(10)?,"taskRevision":row.get::<_,u64>(11)?}),
                task,
            }));
        }
        Ok(None)
    }

    /// Failed preflight is observable without charging a model Run. Version and
    /// queued checks prevent overwriting concurrent cancellation or task edits.
    pub(crate) fn block_work(&mut self, epoch: &str, work: &Work, reason: &str) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, false)?;
        let stopping: bool = tx.query_row(
            "SELECT stop_requested FROM runtime WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if stopping {
            return Ok(());
        }
        let state: Option<(String, u64)> = tx
            .query_row(
                "SELECT status,revision FROM deliveries WHERE id=?1",
                [&work.delivery_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if state != Some(("queued".into(), work.revision)) {
            return Ok(());
        }
        let mut task: Task = load(&tx, "tasks", &work.task.id)?;
        let expired = task.revision != work.message_revision
            || task.revision != work.task.revision
            || task.state == "closed"
            || task.cancellation_requested;
        tx.execute(
            "UPDATE deliveries SET status=?2,reason=?3,revision=revision+1 WHERE id=?1",
            params![
                work.delivery_id,
                if expired { "cancelled" } else { "blocked" },
                reason
            ],
        )?;
        let opened = !expired
            && crate::recovery::needs_human_recovery(reason)
            && crate::recovery::ensure(
                &tx,
                &mut task,
                &work.delivery_id,
                &format!("preflight:{}:{}", work.delivery_id, work.revision),
                reason,
            )?;
        if !expired && !opened {
            let mut recipients = vec![task.team_snapshot.acceptor.clone()];
            // The failed leader already has its blocked delivery. A failure of
            // handling a failure must not recursively generate more model work.
            if task.team_snapshot.leader != work.worker_id && work.message["kind"] != "failure" {
                recipients.push(task.team_snapshot.leader.clone());
            }
            recipients.sort();
            recipients.dedup();
            for recipient in recipients {
                let event = format!("preflight-blocked:{}:{recipient}", work.delivery_id);
                let body = json!({"deliveryId":work.delivery_id,"reason":reason}).to_string();
                enqueue_system(
                    &tx,
                    &mut task,
                    &work.delivery_id,
                    Message {
                        recipient: &recipient,
                        kind: "failure",
                        body: &body,
                        reply_to: None,
                        event: Some(&event),
                        mandatory: true,
                    },
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Command, Team};
    use std::collections::BTreeMap;

    #[test]
    fn stop_during_preflight_preserves_queued_message_without_failure_or_run() {
        let directory = tempfile::tempdir().unwrap();
        let initialized = Store::init(directory.path(), "本人").unwrap();
        let human = initialized["self"]["id"].as_str().unwrap().to_string();
        let mut store = Store::open(directory.path()).unwrap();
        let worker = store
            .execute(
                "worker",
                &Command::WorkerCreate {
                    name: "负责人".into(),
                    description: String::new(),
                    connection: None,
                },
            )
            .unwrap();
        let leader = worker["id"].as_str().unwrap().to_string();
        let team = store
            .execute(
                "team",
                &Command::TeamCreate {
                    team: Team {
                        id: String::new(),
                        name: "团队".into(),
                        leader: leader.clone(),
                        executor: None,
                        verifier: None,
                        deployer: None,
                        acceptor: human.clone(),
                        members: vec![human, leader.clone()],
                        grants: BTreeMap::new(),
                        revision: 1,
                        authorization_revision: 1,
                    },
                },
            )
            .unwrap();
        let created = store
            .execute(
                "task",
                &Command::TaskCreate {
                    team_id: team["id"].as_str().unwrap().into(),
                    goal: "检查停服边界".into(),
                    deploy_environment: None,
                },
            )
            .unwrap();
        store.runtime_register("service", 123).unwrap();
        let work = store.next_work("service").unwrap().unwrap();
        store.runtime_request_stop("service").unwrap();
        assert!(store.next_work("service").unwrap().is_none());
        store
            .block_work("service", &work, "恰在停服时发现缺配置")
            .unwrap();
        assert_eq!(store.mailbox(Some(&leader)).unwrap()[0]["status"], "queued");
        assert!(store.mailbox(None).unwrap().as_array().unwrap().is_empty());
        assert_eq!(
            store
                .task(created["task"]["id"].as_str().unwrap())
                .unwrap()
                .runs_used,
            0
        );
        store.runtime_stopped("service").unwrap();
    }
}
