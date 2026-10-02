//! Submission is model intent; fixed content is published only by the trusted
//! resource owner after all writers and in-flight core operations have stopped.
use crate::{
    Error, Result,
    content::{self, FileEntry, INPUT_LIMIT},
    model::*,
    store::{Message, Store, enqueue_system, load, require, save_task, text},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub handoff_id: Option<String>,
    pub handoff_error: Option<String>,
    pub id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub run_id: String,
    pub worker_id: String,
    pub summary: String,
    pub partial: bool,
    pub content_digest: String,
    pub total_bytes: u64,
    pub files: Vec<FileEntry>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Submission {
    handoff: Option<String>,
    run_id: String,
    task_revision: u64,
    summary: String,
}

pub(crate) fn has_submission(db: &Connection, run: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM artifact_submissions WHERE run_id=?1)",
        [run],
        |r| r.get(0),
    )?)
}
pub(crate) fn submit(
    db: &Connection,
    run: &Run,
    task: &Task,
    summary: &str,
    handoff: Option<&str>,
) -> Result<Value> {
    if !matches!(run.purpose.as_str(), "execute" | "rework")
        || task.state != "active"
        || task.team_snapshot.executor.as_deref() != Some(run.worker_id.as_str())
        || !run.permissions.contains(&Permission::Execute)
    {
        return Err(Error::Forbidden(
            "只有当前冻结执行成员的执行 Run 可提交产出".into(),
        ));
    }
    require(
        &load::<Team>(db, "teams", &task.team_id)?,
        &run.worker_id,
        Permission::Execute,
    )?;
    text(summary, "产出说明", 65536)?;
    if let Some(instruction) = handoff {
        text(instruction, "交接说明", 65536)?;
        if !run.permissions.contains(&Permission::Handoff) {
            return Err(Error::Forbidden("本 Run 未获准直接交接".into()));
        }
        require(
            &load::<Team>(db, "teams", &task.team_id)?,
            &run.worker_id,
            Permission::Handoff,
        )?;
        require(&task.team_snapshot, &run.worker_id, Permission::Handoff)?;
    }
    if has_submission(db, &run.id)? {
        return Err(Error::Conflict(
            "本 Run 已提交产出说明，请结束本轮等待固定".into(),
        ));
    }
    let submission = Submission {
        run_id: run.id.clone(),
        task_revision: task.revision,
        summary: summary.into(),
        handoff: handoff.map(str::to_string),
    };
    db.execute(
        "INSERT INTO artifact_submissions(run_id,data) VALUES(?1,?2)",
        params![run.id, serde_json::to_string(&submission)?],
    )?;
    Ok(
        json!({"submissionId":run.id,"state":"submitted","next":"结束本轮；资源停止并固定内容前，没有可交接产出，也不代表投递已处理"}),
    )
}

/// No model-selected paths. Callers must be trusted resource owners, and must
/// confirm all candidate writers are stopped before calling fix().
pub struct ArtifactPreparation {
    workspace: PathBuf,
    run: Run,
    prior_artifact: Option<String>,
    candidate: Option<crate::candidate::Candidate>,
}
pub struct PreparedArtifact {
    run: Run,
    prior_artifact: Option<String>,
    files: Vec<FileEntry>,
    total_bytes: u64,
    content_digest: String,
    candidate_revision: Option<u64>,
}
fn directory(path: &Path) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(Error::Invalid("候选目录不能为符号链接或特殊文件".into()));
    }
    Ok(())
}
fn candidate(workspace: &Path, run: &str) -> Result<PathBuf> {
    uuid::Uuid::parse_str(run).map_err(|_| Error::Invalid("Run 标识无效".into()))?;
    Ok(workspace.join("candidates").join(run))
}
impl ArtifactPreparation {
    /// Blocking file work: execute outside both the event loop and DB thread.
    pub fn fix(self) -> Result<PreparedArtifact> {
        if let Some(candidate) = self.candidate {
            let total_bytes = crate::candidate::manifest(&candidate.files)?;
            for file in &candidate.files {
                crate::candidate::read_blob(&self.workspace, file)?;
            }
            let content_digest = content::digest(&serde_json::to_vec(&candidate.files)?);
            return Ok(PreparedArtifact {
                candidate_revision: Some(candidate.revision),
                run: self.run,
                prior_artifact: self.prior_artifact,
                files: candidate.files,
                total_bytes,
                content_digest,
            });
        }
        directory(&self.workspace.join("candidates"))?;
        let root = candidate(&self.workspace, &self.run.id)?;
        directory(&root)?;
        let mut pending = vec![root.clone()];
        let mut files = Vec::new();
        let mut total_bytes = 0;
        let mut entries = 0usize;
        let deadline = Instant::now() + Duration::from_secs(120);
        while let Some(dir) = pending.pop() {
            directory(&dir)?;
            for entry in fs::read_dir(&dir)? {
                if Instant::now() >= deadline {
                    return Err(Error::Unavailable("固定产出超时".into()));
                }
                entries += 1;
                if entries > 10000 {
                    return Err(Error::Invalid("候选文件与目录数超过 10000".into()));
                }
                let path = entry?.path();
                let relative = path
                    .strip_prefix(&root)
                    .map_err(|_| Error::Invalid("候选越界".into()))?
                    .to_str()
                    .ok_or_else(|| Error::Invalid("候选路径须为 UTF-8".into()))?
                    .to_string();
                content::relative_path(&relative)?;
                let metadata = fs::symlink_metadata(&path)?;
                if metadata.file_type().is_symlink() {
                    return Err(Error::Invalid("产出不接受符号链接".into()));
                }
                if metadata.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !metadata.is_file() {
                    return Err(Error::Invalid("产出不接受特殊文件".into()));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.nlink() != 1 {
                        return Err(Error::Invalid("候选文件不能是硬链接".into()));
                    }
                }
                let mut bytes = Vec::new();
                fs::File::open(&path)?
                    .take(INPUT_LIMIT - total_bytes + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > INPUT_LIMIT - total_bytes {
                    return Err(Error::Invalid("单产出超过 50 MiB".into()));
                }
                if metadata.len() != bytes.len() as u64 {
                    return Err(Error::Conflict("资源停止后候选仍在变化".into()));
                }
                total_bytes += bytes.len() as u64;
                let sha256 = content::store_blob(&self.workspace, &bytes)?;
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o111 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                files.push(FileEntry {
                    path: relative,
                    sha256,
                    size: bytes.len() as u64,
                    executable,
                });
            }
        }
        if files.is_empty() {
            return Err(Error::Invalid(
                "没有可固定的产出文件；保留运行失败或阻塞事实".into(),
            ));
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let content_digest = content::digest(&serde_json::to_vec(&files)?);
        Ok(PreparedArtifact {
            candidate_revision: None,
            run: self.run,
            prior_artifact: self.prior_artifact,
            files,
            total_bytes,
            content_digest,
        })
    }
}
impl Store {
    pub fn artifact(&self, id: &str) -> Result<Artifact> {
        load(&self.connection, "artifacts", id)
    }
    pub fn artifact_for_run(&self, id: &str) -> Result<Option<Artifact>> {
        let data: Option<String> = self
            .connection
            .query_row("SELECT data FROM artifacts WHERE run_id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        data.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    /// Service-only preparation of a Run-owned directory, never a member tool.
    pub fn runtime_candidate_directory(&self, epoch: &str, id: &str) -> Result<PathBuf> {
        crate::runs::service(&self.connection, epoch, true)?;
        let run = self.run(id)?;
        if run.epoch != epoch
            || run.state != "prepared"
            || run.launch_started
            || !matches!(run.purpose.as_str(), "execute" | "rework")
        {
            return Err(Error::Conflict(
                "只可为本服务尚未启动的执行 Run 准备候选".into(),
            ));
        }
        candidate(&self.workspace_path, id)
    }
    /// Trusted resource inspection must precede this call. No CLI shortcut.
    pub fn runtime_prepare_artifact(&self, epoch: &str, id: &str) -> Result<ArtifactPreparation> {
        crate::runs::service(&self.connection, epoch, false)?;
        let run = self.run(id)?;
        if !matches!(run.purpose.as_str(), "execute" | "rework") {
            return Err(Error::Forbidden("协调和检验 Run 不能固定执行产出".into()));
        }
        let task = self.task(&run.task_id)?;
        Ok(ArtifactPreparation {
            workspace: self.workspace_path.clone(),
            candidate: crate::candidate::candidate(&self.connection, id)?,
            run,
            prior_artifact: task.current_artifact,
        })
    }
    /// After fix(), commit the immutable reference, result message and observed
    /// resource stop together. Revocation/cancellation can only preserve history.
    pub fn runtime_publish_artifact(
        &mut self,
        epoch: &str,
        prepared: PreparedArtifact,
        reason: &str,
    ) -> Result<Artifact> {
        text(reason, "资源停止核对依据", 65536)?;
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, false)?;
        let mut run: Run = load(&tx, "runs", &prepared.run.id)?;
        let prior: Option<String> = tx
            .query_row(
                "SELECT data FROM artifacts WHERE run_id=?1",
                [&run.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(prior) = prior {
            return Ok(serde_json::from_str(&prior)?);
        }
        if run.task_id != prepared.run.task_id
            || run.worker_id != prepared.run.worker_id
            || run.task_revision != prepared.run.task_revision
        {
            return Err(Error::Conflict("产出固定期间 Run 依据变化".into()));
        }
        let mut task: Task = load(&tx, "tasks", &run.task_id)?;
        if crate::candidate::candidate(&tx, &run.id)?.map(|c| c.revision)
            != prepared.candidate_revision
        {
            return Err(Error::Conflict(
                "固定期间候选版本已变化，不能发布旧快照".into(),
            ));
        }
        if task.current_artifact != prepared.prior_artifact {
            return Err(Error::Conflict("已有更新产出，不能覆盖".into()));
        }
        let another: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE task_id=?1 AND id!=?2 AND state!='stopped')",
            params![task.id, run.id],
            |r| r.get(0),
        )?;
        if another {
            return Err(Error::Conflict(
                "同任务已有后续活动或未知 Run，不能发布旧执行结果".into(),
            ));
        }
        let current: Team = load(&tx, "teams", &task.team_id)?;
        let publishable = !run.authority_revoked
            && task.state == "active"
            && !task.cancellation_requested
            && task.revision == run.task_revision
            && run.permissions.contains(&Permission::Execute)
            && [Permission::Execute, Permission::Communicate]
                .iter()
                .all(|p| {
                    require(&current, &run.worker_id, p.clone()).is_ok()
                        && require(&task.team_snapshot, &run.worker_id, p.clone()).is_ok()
                });
        let submitted: Option<String> = tx
            .query_row(
                "SELECT data FROM artifact_submissions WHERE run_id=?1",
                [&run.id],
                |r| r.get(0),
            )
            .optional()?;
        let submitted = submitted
            .map(|s| serde_json::from_str::<Submission>(&s))
            .transpose()?;
        let complete = publishable
            && submitted
                .as_ref()
                .is_some_and(|s| s.run_id == run.id && s.task_revision == task.revision);
        let id = content::digest(&serde_json::to_vec(&(&run.id, &prepared.content_digest))?);
        let mut artifact = Artifact {
            handoff_id: None,
            handoff_error: None,
            id: id.clone(),
            task_id: run.task_id.clone(),
            task_revision: run.task_revision,
            run_id: run.id.clone(),
            worker_id: run.worker_id.clone(),
            summary: submitted.as_ref().map_or_else(
                || "运行停止前未提交正式产出说明".into(),
                |s| s.summary.clone(),
            ),
            partial: !complete,
            content_digest: prepared.content_digest,
            total_bytes: prepared.total_bytes,
            files: prepared.files,
        };
        tx.execute(
            "INSERT INTO artifacts(id,run_id,task_id,data) VALUES(?1,?2,?3,?4)",
            params![id, run.id, task.id, serde_json::to_string(&artifact)?],
        )?;
        if publishable {
            crate::acceptance::supersede(&tx, &task.id, "产出版本已更新，旧验收请求失效")?;
            task.current_artifact = Some(id.clone());
            save_task(&tx, &task)?;
            if complete {
                if let Some(instruction) = submitted.as_ref().and_then(|s| s.handoff.as_deref()) {
                    tx.execute_batch("SAVEPOINT publish_handoff;")?;
                    match crate::handoff::offer(
                        &tx,
                        &run.worker_id,
                        &run.id,
                        &task.id,
                        task.revision,
                        &id,
                        instruction,
                    ) {
                        Ok(result) => {
                            artifact.handoff_id =
                                Some(result["handoff"]["id"].as_str().unwrap().into())
                        }
                        Err(
                            error @ (Error::Invalid(_)
                            | Error::Conflict(_)
                            | Error::Forbidden(_)
                            | Error::NotFound(_)),
                        ) => {
                            tx.execute_batch("ROLLBACK TO publish_handoff;")?;
                            artifact.handoff_error = Some(error.to_string());
                        }
                        Err(error) => return Err(error),
                    }
                    tx.execute_batch("RELEASE publish_handoff;")?;
                    tx.execute(
                        "UPDATE artifacts SET data=?2 WHERE id=?1",
                        params![id, serde_json::to_string(&artifact)?],
                    )?;
                    task = load(&tx, "tasks", &task.id)?;
                }
            }
            let leader = task.team_snapshot.leader.clone();
            enqueue_system(&tx,&mut task,&run.id,Message {recipient:&leader,kind:"result",body:&json!({"artifactId":id,"runId":run.id,"partial":artifact.partial,"summary":artifact.summary,"handoffId":artifact.handoff_id,"handoffError":artifact.handoff_error,"next":"由有权成员决定交接、返工或等待；产出不等于检验通过"}).to_string(),reply_to:None,event:Some(&format!("artifact:{}:{leader}",run.id)),mandatory:true})?;
        }
        if complete {
            crate::disposition::record(
                &tx,
                &run,
                &task,
                json!({"kind":"artifact","artifactId":id}),
            )?;
        }
        crate::runs::stopped(&tx, &mut run, reason)?;
        tx.commit()?;
        Ok(artifact)
    }
}
