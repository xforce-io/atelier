//! An in-process bridge only. Durable work messages live in SQLite.
use crate::{Error, Result, store::Store};
use std::{path::PathBuf, thread};
use tokio::sync::{mpsc, oneshot};

enum Job {
    Operation(Box<dyn FnOnce(&mut Store) + Send>),
    Shutdown,
}

pub struct Database {
    sender: mpsc::Sender<Job>,
    thread: thread::JoinHandle<()>,
}

#[derive(Clone)]
pub struct DatabaseClient {
    sender: mpsc::Sender<Job>,
}

impl Database {
    pub async fn open(path: PathBuf, capacity: usize) -> Result<Self> {
        if capacity == 0 {
            return Err(Error::Invalid("数据库请求容量须大于零".into()));
        }
        let (sender, mut receiver) = mpsc::channel::<Job>(capacity);
        let (ready, startup) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("atelier-database".into())
            .spawn(move || {
                let mut store = match Store::open(&path) {
                    Ok(store) => {
                        if ready.send(Ok(())).is_err() {
                            return;
                        }
                        store
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                while let Some(job) = receiver.blocking_recv() {
                    match job {
                        Job::Operation(operation) => operation(&mut store),
                        Job::Shutdown => receiver.close(),
                    }
                }
            })?;
        startup
            .await
            .map_err(|_| Error::Unavailable("数据库线程启动失败".into()))??;
        Ok(Self { sender, thread })
    }

    pub fn client(&self) -> DatabaseClient {
        DatabaseClient {
            sender: self.sender.clone(),
        }
    }

    /// Close admission and drain accepted jobs, even if clients retain handles.
    pub async fn close(self) -> Result<()> {
        let _ = self.sender.send(Job::Shutdown).await;
        drop(self.sender);
        tokio::task::spawn_blocking(move || self.thread.join())
            .await
            .map_err(|_| Error::Unavailable("数据库线程等待失败".into()))?
            .map_err(|_| Error::Unavailable("数据库线程异常退出".into()))
    }
}

impl DatabaseClient {
    pub async fn runtime_prepare_candidate(&self, epoch: String, run_id: String) -> Result<()> {
        let owner = epoch.clone();
        if let Some(plan) = self
            .call(move |store| store.prepare_candidate(&owner, &run_id))
            .await?
        {
            let prepared = tokio::task::spawn_blocking(move || plan.validate())
                .await
                .map_err(|_| Error::Unavailable("候选准备线程退出".into()))??;
            self.call(move |store| store.publish_candidate(&epoch, prepared))
                .await?;
        }
        Ok(())
    }
    /// Reconcile only owned check containers from older service epochs. A Run
    /// remains unknown until its other resources have also been inspected.
    pub async fn runtime_recover_checks(&self, epoch: String) -> Result<usize> {
        let owner = epoch.clone();
        let plans = self
            .call(move |store| store.interrupted_checks(&owner))
            .await?;
        let mut stopped = 0;
        for plan in plans {
            let observed = crate::checker::recover(plan).await?;
            let owner = epoch.clone();
            let check = self
                .call(move |store| store.finish_check(&owner, observed))
                .await?;
            stopped += usize::from(check.resources_stopped);
        }
        Ok(stopped)
    }
    /// Resource-owner only: all candidate writers and accepted member calls must
    /// already be stopped/drained. File hashing never holds the DB thread.
    pub async fn runtime_finish_execution(
        &self,
        epoch: String,
        run_id: String,
        reason: String,
    ) -> Result<Option<crate::artifact::Artifact>> {
        let owner = epoch.clone();
        let id = run_id.clone();
        let observed = reason.clone();
        if self
            .call(move |store| store.runtime_finish_empty_blocked_execution(&owner, &id, &observed))
            .await?
        {
            return Ok(None);
        }
        self.runtime_fix_artifact(epoch, run_id, reason)
            .await
            .map(Some)
    }
    pub async fn runtime_fix_artifact(
        &self,
        epoch: String,
        run_id: String,
        reason: String,
    ) -> Result<crate::artifact::Artifact> {
        let id = run_id.clone();
        let owner = epoch.clone();
        if let Some(artifact) = self
            .call(move |store| {
                crate::runs::service(&store.connection, &owner, false)?;
                store.artifact_for_run(&id)
            })
            .await?
        {
            return Ok(artifact);
        }
        let owner = epoch.clone();
        let preparation = self
            .call(move |store| store.runtime_prepare_artifact(&owner, &run_id))
            .await?;
        let fixed = tokio::task::spawn_blocking(move || preparation.fix())
            .await
            .map_err(|_| Error::Unavailable("产出固定线程退出".into()))??;
        self.call(move |store| store.runtime_publish_artifact(&epoch, fixed, &reason))
            .await
    }
    /// Hashing input files runs outside both the event loop and SQLite thread.
    /// Publication rechecks the bound Run, task version and immutable input ID.
    pub async fn member_call(
        &self,
        binding: crate::member::MemberBinding,
        operation: crate::member::ToolOperation,
    ) -> Result<serde_json::Value> {
        let prepared = if operation.name == "task_intake" {
            let bound = binding.clone();
            let request = operation.clone();
            self.call(move |store| store.prepare_member_intake(&bound, &request))
                .await?
        } else {
            None
        };
        let validated = if let Some(prepared) = prepared {
            Some(
                tokio::task::spawn_blocking(move || prepared.validate())
                    .await
                    .map_err(|_| Error::Unavailable("承接内容核对线程退出".into()))??,
            )
        } else {
            None
        };
        let bound = binding.clone();
        let request = operation.clone();
        let file = if crate::candidate::is_tool(&operation.name) {
            let b = binding.clone();
            let o = operation.clone();
            if let Some(plan) = self
                .call(move |store| store.prepare_file_tool(&b, &o))
                .await?
            {
                Some(
                    tokio::task::spawn_blocking(move || plan.prepare())
                        .await
                        .map_err(|_| Error::Unavailable("文件准备线程退出".into()))??,
                )
            } else {
                None
            }
        } else {
            None
        };
        let result = self
            .call(move |store| {
                store.member_call_prepared(&bound, &request, validated.as_ref(), file.as_ref())
            })
            .await?;
        if operation.name == "run_check" && result["ok"] == true {
            let id = result["data"]["check"]["id"]
                .as_str()
                .ok_or_else(|| Error::Invalid("检查提交结果缺失".into()))?
                .to_string();
            let bound = binding.clone();
            if let Some(plan) = self
                .call(move |store| store.begin_check(&bound, &id))
                .await?
            {
                let epoch = plan.epoch.clone();
                let observed = crate::checker::execute(plan, self.clone()).await?;
                self.call(move |store| store.finish_check(&epoch, observed))
                    .await?;
            }
            return self
                .call(move |store| store.member_call(&binding, &operation))
                .await;
        }
        Ok(result)
    }

    pub async fn call<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Store) -> Result<T> + Send + 'static,
    {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Job::Operation(Box::new(move |store| {
                let value = operation(store);
                // A cancelled waiter does not undo an accepted operation or transaction.
                let _ = reply.send(value);
            })))
            .await
            .map_err(|_| Error::Unavailable("数据库线程已关闭".into()))?;
        result
            .await
            .map_err(|_| Error::Unavailable("数据库回复丢失；写操作请按原 requestId 核对".into()))?
    }
}
