//! Deploy opens after acceptance. The member changes the target with host
//! commands; only a core verification closes the task as deployed.
use crate::{
    Error, Result,
    artifact::Artifact,
    candidate,
    content::{self, FileEntry, GitInput},
    environment,
    model::*,
    store::{Message, enqueue_system, load, require, save_task, text},
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub(crate) fn open_after_acceptance(
    db: &Connection,
    workspace: &Path,
    task: &mut Task,
    acceptance_id: &str,
    cause: &str,
) -> Result<()> {
    let deployer = task
        .team_snapshot
        .deployer
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有冻结部署职责".into()))?;
    let worker = task
        .worker_snapshots
        .get(&deployer)
        .cloned()
        .ok_or_else(|| Error::Conflict("部署成员快照缺失".into()))?;
    if let Some(reason) = block_reason(db, task, &deployer, &worker)? {
        task.deploy = Some(blocked(acceptance_id, reason));
        task.revision += 1;
        return save_task(db, task);
    }
    let snapshot = task
        .environment_snapshot
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有点名部署目标环境".into()))?;
    let export = export_accepted(db, workspace, task, &snapshot, acceptance_id)?;
    let deployer_id = deployer.clone();
    task.deploy = Some(DeployRecord {
        state: "open".into(),
        acceptance_id: acceptance_id.into(),
        reason: None,
        export: Some(export.clone()),
        commands: Vec::new(),
        verifications: Vec::new(),
    });
    task.revision += 1;
    let body = json!({
        "acceptanceId": acceptance_id,
        "environment": snapshot.name,
        "codeRoot": snapshot.code_root,
        "service": snapshot.service,
        "verification": snapshot.verification,
        "exportDir": export.dir,
        "changes": export.changes,
        "instruction": "用 host_exec 完成部署，做完用 deploy_verify 请核心核对；不能自报结果。"
    })
    .to_string();
    enqueue_system(
        db,
        task,
        cause,
        Message {
            recipient: &deployer_id,
            kind: "assignment.deploy",
            body: &body,
            reply_to: None,
            event: Some(&format!("deploy:{acceptance_id}")),
            mandatory: false,
        },
    )?;
    Ok(())
}

fn blocked(acceptance_id: &str, reason: String) -> DeployRecord {
    DeployRecord {
        state: "blocked".into(),
        acceptance_id: acceptance_id.into(),
        reason: Some(reason),
        export: None,
        commands: Vec::new(),
        verifications: Vec::new(),
    }
}

fn block_reason(
    db: &Connection,
    task: &Task,
    deployer: &str,
    worker: &Worker,
) -> Result<Option<String>> {
    if worker.kind == WorkerKind::Agent && task.runs_used >= task.contract.max_runs {
        return Ok(Some("任务 Run 额度耗尽，不能发起部署".into()));
    }
    if task.messages_used >= task.contract.max_messages {
        return Ok(Some("任务消息额度耗尽，不能发起部署".into()));
    }
    let current: Team = load(db, "teams", &task.team_id)?;
    for permission in [Permission::Deploy, Permission::Communicate] {
        if require(&current, deployer, permission.clone()).is_err()
            || require(&task.team_snapshot, deployer, permission).is_err()
        {
            return Ok(Some("部署成员缺少当前或冻结的 task.deploy 授权".into()));
        }
    }
    if worker.kind == WorkerKind::Agent && worker.execution_config.is_none() {
        return Ok(Some("部署成员的冻结执行配置缺失".into()));
    }
    if let Err(error) = crate::blocker::ensure_clear(db, task) {
        return Ok(Some(error.to_string()));
    }
    let Some(snapshot) = &task.environment_snapshot else {
        return Ok(Some("任务没有点名部署目标环境".into()));
    };
    if task.contract.deploy_environment.as_deref() != Some(snapshot.name.as_str()) {
        return Ok(Some("任务没有点名部署目标环境".into()));
    }
    if !environment::unchanged(db, snapshot)? {
        return Ok(Some("部署目标登记已变化".into()));
    }
    let Some(input_id) = &task.contract.code_input else {
        return Ok(Some("任务没有代码输入".into()));
    };
    let artifact = accepted_artifact(db, task)?;
    let baseline: GitInput = load(db, "inputs", input_id)?;
    if change_list(&artifact.files, &baseline.files)?.is_empty() {
        return Ok(Some("产出与基线没有差异".into()));
    }
    Ok(None)
}

fn accepted_artifact(db: &Connection, task: &Task) -> Result<Artifact> {
    let artifact_id = task
        .current_artifact
        .clone()
        .ok_or_else(|| Error::Conflict("验收时没有当前产出".into()))?;
    let artifact: Artifact = load(db, "artifacts", &artifact_id)?;
    if candidate::manifest(&artifact.files)? != artifact.total_bytes
        || content::digest(&serde_json::to_vec(&artifact.files)?) != artifact.content_digest
    {
        return Err(Error::Conflict("产出清单损坏，不能导出".into()));
    }
    Ok(artifact)
}

fn export_accepted(
    db: &Connection,
    workspace: &Path,
    task: &Task,
    snapshot: &Environment,
    acceptance_id: &str,
) -> Result<DeployExport> {
    let _ = snapshot;
    let artifact = accepted_artifact(db, task)?;
    let input_id = task
        .contract
        .code_input
        .clone()
        .ok_or_else(|| Error::Conflict("任务没有代码输入".into()))?;
    let baseline: GitInput = load(db, "inputs", &input_id)?;
    let changes = change_list(&artifact.files, &baseline.files)?;
    if changes.is_empty() {
        return Err(Error::Conflict("产出与基线没有差异".into()));
    }
    let root = workspace.join("deploy-exports");
    fs::create_dir_all(&root)?;
    let final_dir = root.join(acceptance_id);
    if final_dir.exists() {
        return Err(Error::Conflict("部署导出目录已存在".into()));
    }
    let stage = root.join(format!(".tmp-{acceptance_id}"));
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    let copied = (|| -> Result<()> {
        fs::create_dir(&stage)?;
        for file in &artifact.files {
            let bytes = candidate::read_blob(workspace, file)?;
            let path = stage.join(&file.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(if file.executable { 0o755 } else { 0o644 });
            }
            let mut output = options.open(&path)?;
            output.write_all(&bytes)?;
            output.sync_all()?;
        }
        Ok(())
    })();
    if let Err(error) = copied {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    fs::rename(&stage, &final_dir)?;
    lock_tree(&final_dir)?;
    let changes_path = root.join(format!("{acceptance_id}.changes.json"));
    let mut changes_file = fs::File::create(&changes_path)?;
    changes_file.write_all(&serde_json::to_vec(&changes)?)?;
    changes_file.sync_all()?;
    #[cfg(unix)]
    {
        let mut permissions = changes_file.metadata()?.permissions();
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o444);
        fs::set_permissions(&changes_path, permissions)?;
    }
    Ok(DeployExport {
        dir: final_dir.to_string_lossy().into_owned(),
        artifact_id: artifact.id,
        baseline_input_id: input_id,
        changes,
    })
}

fn lock_tree(root: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut directories = vec![root.to_path_buf()];
        let mut files = Vec::new();
        let mut index = 0;
        while index < directories.len() {
            for entry in fs::read_dir(&directories[index])? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                if file_type.is_symlink() {
                    return Err(Error::Conflict("导出结果含符号链接".into()));
                }
                if file_type.is_dir() {
                    directories.push(entry.path());
                } else if file_type.is_file() {
                    files.push(entry.path());
                }
            }
            index += 1;
        }
        for file in files {
            let executable = fs::metadata(&file)?.permissions().mode() & 0o111 != 0;
            let mut permissions = fs::metadata(&file)?.permissions();
            permissions.set_mode(if executable { 0o555 } else { 0o444 });
            fs::set_permissions(&file, permissions)?;
        }
        for directory in directories.iter().rev() {
            let mut permissions = fs::metadata(directory)?.permissions();
            permissions.set_mode(0o555);
            fs::set_permissions(directory, permissions)?;
        }
    }
    Ok(())
}

pub(crate) fn change_list(
    artifact: &[FileEntry],
    baseline: &[FileEntry],
) -> Result<Vec<DeployChange>> {
    fn index(
        files: &[FileEntry],
    ) -> Result<std::collections::BTreeMap<&str, &FileEntry>> {
        let mut map = std::collections::BTreeMap::new();
        for file in files {
            if map.insert(file.path.as_str(), file).is_some() {
                return Err(Error::Conflict("产出或基线含重复路径".into()));
            }
        }
        Ok(map)
    }
    let artifact = index(artifact)?;
    let baseline = index(baseline)?;
    let mut changes = Vec::new();
    for (path, file) in &artifact {
        let same = baseline.get(path).is_some_and(|old| {
            old.sha256 == file.sha256 && old.size == file.size && old.executable == file.executable
        });
        if !same {
            changes.push(DeployChange {
                path: (*path).to_string(),
                action: "write".into(),
                sha256: Some(file.sha256.clone()),
                executable: Some(file.executable),
            });
        }
    }
    for path in baseline.keys() {
        if !artifact.contains_key(path) {
            changes.push(DeployChange {
                path: (*path).to_string(),
                action: "delete".into(),
                sha256: None,
                executable: None,
            });
        }
    }
    Ok(changes)
}

pub(crate) fn close_succeeded(db: &Connection, task: &mut Task, reason: &str) -> Result<()> {
    text(reason, "部署核对依据", 65536)?;
    let mut deploy = task
        .deploy
        .clone()
        .ok_or_else(|| Error::Conflict("部署尚未打开".into()))?;
    if deploy.state != "open" {
        return Err(Error::Conflict("部署结果已结束或尚未开放".into()));
    }
    finish_unclaimed_assignment(db, &task.id, reason)?;
    deploy.state = "succeeded".into();
    deploy.reason = Some(reason.into());
    task.deploy = Some(deploy);
    task.state = "closed".into();
    task.outcome = Some("deployed".into());
    task.revision += 1;
    save_task(db, task)?;
    crate::decisions::supersede(db, &task.id, None, task.revision, "部署核对通过，任务关闭")?;
    crate::blocker::supersede(db, &task.id, "部署核对通过，任务关闭")?;
    db.execute("UPDATE deliveries SET status='cancelled',reason='部署核对通过，任务关闭',revision=revision+1 WHERE status IN ('queued','blocked') AND message_id IN (SELECT id FROM messages WHERE task_id=?1)", [&task.id])?;
    Ok(())
}

fn finish_unclaimed_assignment(db: &Connection, task_id: &str, reason: &str) -> Result<()> {
    db.execute(
        "UPDATE deliveries SET status='handled',reason=?2,revision=revision+1 WHERE run_id IS NULL AND status IN ('queued','blocked') AND message_id IN (SELECT id FROM messages WHERE task_id=?1 AND kind='assignment.deploy')",
        params![task_id, reason],
    )?;
    Ok(())
}

pub(crate) fn deploy_mut(task: &mut Task) -> Result<&mut DeployRecord> {
    task.deploy
        .as_mut()
        .filter(|record| record.state == "open")
        .ok_or_else(|| Error::Conflict("部署尚未开放".into()))
}

pub(crate) fn changes_file(export_dir: &str) -> PathBuf {
    let dir = PathBuf::from(export_dir);
    let name = dir
        .file_name()
        .map(|name| {
            let mut file = name.to_os_string();
            file.push(".changes.json");
            file
        })
        .unwrap_or_default();
    dir.parent().unwrap_or(Path::new(".")).join(name)
}
