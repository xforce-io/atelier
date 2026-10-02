//! Durable native-context binding. Files remain owned by the adapter; the core
//! never reconstructs a missing checkpoint from messages or model output.
use crate::{
    Error, Result,
    content::digest,
    model::{Run, Task},
    store::{Store, load, new_id, save},
};
use rusqlite::{OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionContext {
    pub id: String,
    pub task_id: String,
    pub worker_id: String,
    pub configuration_id: String,
    pub purpose_family: String,
    pub used: bool,
    key: String,
}

fn family(purpose: &str) -> Result<&str> {
    match purpose {
        "coordinate" | "verify" => Ok(purpose),
        "execute" | "rework" => Ok("execute"),
        _ => Err(Error::Invalid("未知执行用途".into())),
    }
}
fn context_key(
    task: &Task,
    worker_id: &str,
    configuration_id: &str,
    purpose: &str,
) -> Result<String> {
    let worker = task
        .worker_snapshots
        .get(worker_id)
        .ok_or_else(|| Error::Forbidden("成员不在任务快照中".into()))?;
    if worker.execution_config.as_deref() != Some(configuration_id) {
        return Err(Error::Conflict("执行配置与任务快照不符".into()));
    }
    let team = &task.team_snapshot;
    // Task revision and contract edits do not discard history. An incompatible
    // frozen member/role/permission snapshot selects a separate context.
    Ok(digest(&serde_json::to_vec(&(
        &task.id,
        worker_id,
        configuration_id,
        family(purpose)?,
        &worker.name,
        &worker.description,
        &team.members,
        &team.leader,
        &team.executor,
        &team.verifier,
        &team.acceptor,
        &team.grants,
    ))?))
}

impl ExecutionContext {
    pub fn directories(&self, workspace: &Path) -> (PathBuf, PathBuf) {
        let root = workspace.join("contexts").join(&self.id);
        (root.join("native"), root.join("ledger"))
    }

    /// May create only a never-used reservation. Once start might have reached
    /// the child, missing files are a recovery failure, never an empty session.
    pub fn prepare_directories(&self, workspace: &Path) -> Result<()> {
        let root = workspace.join("contexts");
        let own = root.join(&self.id);
        let (native, ledger) = self.directories(workspace);
        for path in [&root, &own, &native, &ledger] {
            private_directory(path, !self.used)?;
        }
        if self.used {
            for name in ["binding.json", "state.sqlite3"] {
                let metadata = fs::symlink_metadata(native.join(name))
                    .map_err(|_| Error::Unavailable("原生上下文丢失，须核对后恢复".into()))?;
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err(Error::Unavailable(
                        "原生上下文文件无效，须核对后恢复".into(),
                    ));
                }
            }
            private_directory(&native.join("events"), false)?;
        } else if fs::read_dir(&native)?.next().is_some() || fs::read_dir(&ledger)?.next().is_some()
        {
            return Err(Error::Unavailable(
                "未启动上下文出现已有内容，须核对后恢复".into(),
            ));
        }
        Ok(())
    }
}

fn private_directory(path: &Path, create: bool) -> Result<()> {
    if create {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(path) {
            Ok(()) => {
                fs::File::open(path)?.sync_all()?;
                if let Some(parent) = path.parent() {
                    fs::File::open(parent)?.sync_all()?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| Error::Unavailable("原生上下文目录丢失，须核对后恢复".into()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(Error::Unavailable("原生上下文目录无效".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::Unavailable("原生上下文目录须为私有目录".into()));
        }
    }
    Ok(())
}

impl Store {
    /// Service-only reservation, before resources start. Does not consume Run
    /// budget and does not create an alternative task or delivery queue.
    pub fn runtime_context(
        &mut self,
        epoch: &str,
        task_id: &str,
        worker_id: &str,
        configuration_id: &str,
        purpose: &str,
    ) -> Result<ExecutionContext> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, true)?;
        let task: Task = load(&tx, "tasks", task_id)?;
        if task.state == "closed" || task.cancellation_requested {
            return Err(Error::Conflict("任务已关闭或正在取消".into()));
        }
        let key = context_key(&task, worker_id, configuration_id, purpose)?;
        let saved: Option<String> = tx
            .query_row(
                "SELECT data FROM execution_contexts WHERE id=?1",
                [&key],
                |r| r.get(0),
            )
            .optional()?;
        let context = if let Some(saved) = saved {
            serde_json::from_str(&saved)?
        } else {
            let context = ExecutionContext {
                id: new_id(),
                task_id: task_id.into(),
                worker_id: worker_id.into(),
                configuration_id: configuration_id.into(),
                purpose_family: family(purpose)?.into(),
                used: false,
                key,
            };
            save(&tx, "execution_contexts", &context.key, &context)?;
            context
        };
        tx.commit()?;
        Ok(context)
    }

    /// Atomically records context use and launch intent before spawning. No
    /// failure after this point may silently turn resume back into fresh start.
    pub fn runtime_begin_api_launch(
        &mut self,
        epoch: &str,
        run_id: &str,
        context: &ExecutionContext,
        credential_generation: u64,
    ) -> Result<Run> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, true)?;
        let mut run: Run = load(&tx, "runs", run_id)?;
        if run.epoch != epoch || run.state != "prepared" || run.launch_started {
            return Err(Error::Conflict(
                "Run 不处于本 epoch 的未启动准备状态".into(),
            ));
        }
        crate::runs::check_run_authority(&tx, &run)?;
        let task: Task = load(&tx, "tasks", &run.task_id)?;
        let key = context_key(&task, &run.worker_id, &run.configuration_id, &run.purpose)?;
        let mut current: ExecutionContext = load(&tx, "execution_contexts", &key)?;
        if current.id != context.id
            || current.used != context.used
            || task.revision != run.task_revision
        {
            return Err(Error::Conflict("上下文或任务依据已变化".into()));
        }
        let configuration: crate::connection::ExecutionConfiguration =
            load(&tx, "execution_configs", &run.configuration_id)?;
        let reference: String = tx
            .query_row(
                "SELECT data FROM credentials WHERE version_id=?1",
                [&configuration.connection_version],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::Conflict("API 凭据已移除".into()))?;
        let reference: crate::credential::CredentialReference = serde_json::from_str(&reference)?;
        if reference.generation != credential_generation {
            return Err(Error::Conflict("API 凭据代次已变化".into()));
        }
        current.used = true;
        save(&tx, "execution_contexts", &key, &current)?;
        let record = serde_json::json!({"contextId":current.id,"resume":context.used,"credentialGeneration":credential_generation});
        tx.execute(
            "INSERT INTO api_launches(run_id,data) VALUES(?1,?2)",
            rusqlite::params![run_id, record.to_string()],
        )?;
        run.launch_started = true;
        crate::runs::save(&tx, &run)?;
        tx.commit()?;
        Ok(run)
    }
}
