//! Member file access uses immutable blobs and an atomic per-Run manifest.
//! No model path is ever joined to a host candidate directory.
use crate::{
    Error, Result,
    content::{self, FileEntry},
    member::{MemberBinding, ToolOperation, bound_run},
    model::{Permission, Run, Task},
    store::{Store, load},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Candidate {
    pub run_id: String,
    pub revision: u64,
    pub files: Vec<FileEntry>,
}
#[derive(Clone, Deserialize)]
#[serde(
    tag = "name",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum FileCommand {
    ListFiles(ListFiles),
    ReadFile(ReadFile),
    WriteFile(WriteFile),
    DeleteFile(FilePath),
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ListFiles {
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub offset: usize,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ReadFile {
    pub path: String,
    #[serde(default)]
    pub offset: usize,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct WriteFile {
    pub path: String,
    pub content: String,
    pub executable: Option<bool>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FilePath {
    pub path: String,
}
#[derive(Clone)]
pub(crate) struct View {
    pub id: String,
    pub revision: u64,
    pub files: Vec<FileEntry>,
}
pub(crate) struct Preparation {
    workspace: PathBuf,
    run_id: String,
    command: FileCommand,
    view: View,
}
pub(crate) struct Prepared {
    run_id: String,
    view_id: String,
    revision: u64,
    input: Vec<u8>,
    result: Result<Value>,
    files: Option<Vec<FileEntry>>,
}
pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "list_files" | "read_file" | "write_file" | "delete_file"
    )
}
fn parse(op: &ToolOperation) -> Result<FileCommand> {
    serde_json::from_value(json!({"name":op.name,"input":op.input}))
        .map_err(|_| Error::Invalid("文件工具参数无效".into()))
}
fn input_key(command: &FileCommand) -> Vec<u8> {
    // The actual operation fingerprint is checked by the common member ledger;
    // this key additionally binds an out-of-thread preparation to its command.
    match command {
        FileCommand::ListFiles(p) => serde_json::to_vec(&json!(["list", p.prefix, p.offset])),
        FileCommand::ReadFile(p) => serde_json::to_vec(&json!(["read", p.path, p.offset])),
        FileCommand::WriteFile(p) => {
            serde_json::to_vec(&json!(["write", p.path, p.content, p.executable]))
        }
        FileCommand::DeleteFile(p) => serde_json::to_vec(&json!(["delete", p.path])),
    }
    .expect("serializing file tool values")
}
pub(crate) fn candidate(db: &Connection, run: &str) -> Result<Option<Candidate>> {
    let data: Option<String> = db
        .query_row("SELECT data FROM candidates WHERE run_id=?1", [run], |r| {
            r.get(0)
        })
        .optional()?;
    data.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}
fn view(db: &Connection, run: &Run, task: &Task, write: bool) -> Result<View> {
    if matches!(run.purpose.as_str(), "execute" | "rework")
        && run.permissions.contains(&Permission::Execute)
        && task.team_snapshot.executor.as_deref() == Some(run.worker_id.as_str())
    {
        let c =
            candidate(db, &run.id)?.ok_or_else(|| Error::Conflict("本 Run 候选尚未准备".into()))?;
        return Ok(View {
            id: c.run_id,
            revision: c.revision,
            files: c.files,
        });
    }
    if !write
        && run.purpose == "verify"
        && run.permissions.contains(&Permission::Verify)
        && task.team_snapshot.verifier.as_deref() == Some(run.worker_id.as_str())
    {
        crate::handoff::claimable(db, task, &run.delivery_id)?;
        let data: String = db.query_row(
            "SELECT data FROM handoffs WHERE json_extract(data,'$.delivery_id')=?1",
            [&run.delivery_id],
            |r| r.get(0),
        )?;
        let h: crate::handoff::Handoff = serde_json::from_str(&data)?;
        let a: crate::artifact::Artifact = load(db, "artifacts", &h.artifact_id)?;
        return Ok(View {
            id: a.id,
            revision: a.task_revision,
            files: a.files,
        });
    }
    Err(Error::Forbidden("本 Run 不具备该候选的文件访问权限".into()))
}
fn writing(command: &FileCommand) -> bool {
    matches!(
        command,
        FileCommand::WriteFile(_) | FileCommand::DeleteFile(_)
    )
}
pub(crate) fn manifest(files: &[FileEntry]) -> Result<u64> {
    if files.len() > 10000 || files.windows(2).any(|w| w[0].path >= w[1].path) {
        return Err(Error::Invalid("候选清单超限或次序无效".into()));
    }
    let mut total = 0u64;
    let paths: std::collections::BTreeSet<_> = files.iter().map(|f| f.path.as_str()).collect();
    for f in files {
        content::relative_path(&f.path)?;
        for (i, _) in f.path.match_indices('/') {
            if paths.contains(&f.path[..i]) {
                return Err(Error::Invalid("文件与目录路径冲突".into()));
            }
        }
        total = total
            .checked_add(f.size)
            .ok_or_else(|| Error::Invalid("候选大小超限".into()))?;
        if total > content::INPUT_LIMIT {
            return Err(Error::Invalid("候选超过 50 MiB".into()));
        }
    }
    Ok(total)
}
pub(crate) fn read_blob(workspace: &std::path::Path, f: &FileEntry) -> Result<Vec<u8>> {
    if f.sha256.len() != 64
        || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || f.size > content::INPUT_LIMIT
    {
        return Err(Error::Invalid("文件内容引用无效".into()));
    }
    for dir in ["objects", "objects/blobs"] {
        let m = std::fs::symlink_metadata(workspace.join(dir))?;
        if !m.is_dir() || m.file_type().is_symlink() {
            return Err(Error::Invalid("内容目录无效".into()));
        }
    }
    let path = workspace.join("objects/blobs").join(&f.sha256);
    let m = std::fs::symlink_metadata(&path)?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() != f.size {
        return Err(Error::Conflict("文件内容缺失或损坏".into()));
    }
    let bytes = std::fs::read(path)?;
    if content::digest(&bytes) != f.sha256 {
        return Err(Error::Conflict("文件内容摘要不匹配".into()));
    }
    Ok(bytes)
}
impl Preparation {
    pub(crate) fn prepare(self) -> Result<Prepared> {
        let input = input_key(&self.command);
        let mut files = self.view.files.clone();
        let result = (|| -> Result<Value> {
            manifest(&files)?;
            match &self.command {
                FileCommand::ListFiles(p) => {
                    if !p.prefix.is_empty() {
                        content::relative_path(&p.prefix)?;
                    }
                    let matching: Vec<_> = files
                        .iter()
                        .filter(|f| {
                            p.prefix.is_empty()
                                || f.path == p.prefix
                                || f.path.starts_with(&format!("{}/", p.prefix))
                        })
                        .collect();
                    if p.offset > matching.len() {
                        return Err(Error::Invalid("文件列表偏移超出范围".into()));
                    }
                    let mut end = (p.offset + 20).min(matching.len());
                    while serde_json::to_vec(&matching[p.offset..end])?.len() > 192 * 1024 {
                        end -= 1;
                    }
                    Ok(
                        json!({"files":matching[p.offset..end],"total":matching.len(),"nextOffset":(end<matching.len()).then_some(end)}),
                    )
                }
                FileCommand::ReadFile(p) => {
                    content::relative_path(&p.path)?;
                    let f = files
                        .iter()
                        .find(|f| f.path == p.path)
                        .ok_or_else(|| Error::NotFound("候选文件不存在".into()))?;
                    let bytes = read_blob(&self.workspace, f)?;
                    let text = std::str::from_utf8(&bytes).map_err(|_| {
                        Error::Invalid("该文件不是 UTF-8 文本；不能作为文本读取".into())
                    })?;
                    if p.offset > bytes.len() || !text.is_char_boundary(p.offset) {
                        return Err(Error::Invalid("读取偏移须为有效 UTF-8 字节边界".into()));
                    }
                    let mut end = (p.offset + 32768).min(bytes.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    Ok(
                        json!({"path":p.path,"sha256":f.sha256,"size":f.size,"offset":p.offset,"content":&text[p.offset..end],"nextOffset":(end<bytes.len()).then_some(end)}),
                    )
                }
                FileCommand::WriteFile(p) => {
                    content::relative_path(&p.path)?;
                    if p.content.len() > 256 * 1024 {
                        return Err(Error::Invalid("单次文件写入超过 256 KiB".into()));
                    }
                    let existing = files.iter().find(|f| f.path == p.path);
                    let entry = FileEntry {
                        path: p.path.clone(),
                        sha256: content::digest(p.content.as_bytes()),
                        size: p.content.len() as u64,
                        executable: p
                            .executable
                            .unwrap_or(existing.is_some_and(|f| f.executable)),
                    };
                    files.retain(|f| f.path != p.path);
                    files.push(entry.clone());
                    files.sort_by(|a, b| a.path.cmp(&b.path));
                    manifest(&files)?;
                    content::store_blob(&self.workspace, p.content.as_bytes())?;
                    Ok(json!({"file":entry}))
                }
                FileCommand::DeleteFile(p) => {
                    content::relative_path(&p.path)?;
                    let old = files.len();
                    files.retain(|f| f.path != p.path);
                    if old == files.len() {
                        return Err(Error::NotFound("待删除的候选文件不存在".into()));
                    }
                    Ok(json!({"deleted":p.path}))
                }
            }
        })();
        let result = match result {
            Ok(v) => Ok(v),
            Err(
                e @ (Error::Io(_) | Error::Database(_) | Error::Unavailable(_) | Error::Json(_)),
            ) => return Err(e),
            Err(e) => Err(e),
        };
        Ok(Prepared {
            run_id: self.run_id,
            view_id: self.view.id,
            revision: self.view.revision,
            input,
            result,
            files: writing(&self.command).then_some(files),
        })
    }
}
impl Store {
    pub(crate) fn prepare_file_tool(
        &self,
        binding: &MemberBinding,
        op: &ToolOperation,
    ) -> Result<Option<Preparation>> {
        let (run, task) = bound_run(&self.connection, binding)?;
        let prior: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM member_requests WHERE delivery_id=?1 AND operation_id=?2)",
            params![run.delivery_id, op.operation_id],
            |r| r.get(0),
        )?;
        if prior || serde_json::to_vec(op)?.len() > crate::member::MAX_TOOL_REQUEST {
            return Ok(None);
        }
        let Ok(command) = parse(op) else {
            return Ok(None);
        };
        if writing(&command)
            && (crate::artifact::has_submission(&self.connection, &run.id)?
                || crate::disposition::exists(&self.connection, &run.delivery_id)?)
        {
            return Ok(None);
        }
        let Ok(view) = view(&self.connection, &run, &task, writing(&command)) else {
            return Ok(None);
        };
        Ok(Some(Preparation {
            workspace: self.workspace_path.clone(),
            run_id: run.id,
            command,
            view,
        }))
    }
}
pub(crate) fn effect(
    db: &Connection,
    run: &Run,
    task: &Task,
    op: &ToolOperation,
    prepared: Option<&Prepared>,
) -> Result<Value> {
    let command = parse(op)?;
    let current = view(db, run, task, writing(&command))?;
    let p = prepared
        .ok_or_else(|| Error::Unavailable("文件工具必须通过异步文件准备入口调用".into()))?;
    if p.run_id != run.id
        || p.view_id != current.id
        || p.revision != current.revision
        || p.input != input_key(&command)
    {
        return Err(Error::Conflict(
            "文件准备期间候选版本已变化，请读取最新版本后重试".into(),
        ));
    }
    let mut result = p
        .result
        .as_ref()
        .map_err(|error| match error {
            Error::Invalid(s) => Error::Invalid(s.clone()),
            Error::NotFound(s) => Error::NotFound(s.clone()),
            Error::Conflict(s) => Error::Conflict(s.clone()),
            Error::Forbidden(s) => Error::Forbidden(s.clone()),
            _ => Error::Unavailable("文件准备未完成".into()),
        })?
        .clone();
    let revision = if let Some(files) = &p.files {
        let c = Candidate {
            run_id: run.id.clone(),
            revision: current.revision + 1,
            files: files.clone(),
        };
        db.execute(
            "UPDATE candidates SET data=?2 WHERE run_id=?1",
            params![run.id, serde_json::to_string(&c)?],
        )?;
        c.revision
    } else {
        current.revision
    };
    result["viewId"] = json!(current.id);
    result["revision"] = json!(revision);
    Ok(result)
}

pub(crate) struct CandidatePreparation {
    workspace: PathBuf,
    run: Run,
    input: Option<content::GitInput>,
    artifact: Option<crate::artifact::Artifact>,
    prior_artifact: Option<String>,
}
pub(crate) struct PreparedCandidate {
    plan: CandidatePreparation,
    files: Vec<FileEntry>,
}
impl CandidatePreparation {
    pub(crate) fn validate(self) -> Result<PreparedCandidate> {
        let files = if let Some(input) = &self.input {
            content::validate_input(&self.workspace, input)?;
            input.files.clone()
        } else if let Some(artifact) = &self.artifact {
            if content::digest(&serde_json::to_vec(&artifact.files)?) != artifact.content_digest
                || manifest(&artifact.files)? != artifact.total_bytes
            {
                return Err(Error::Conflict("返工产出清单损坏".into()));
            }
            for f in &artifact.files {
                read_blob(&self.workspace, f)?;
            }
            artifact.files.clone()
        } else {
            return Err(Error::Conflict("没有固定代码输入".into()));
        };
        manifest(&files)?;
        Ok(PreparedCandidate { plan: self, files })
    }
}
impl Store {
    pub(crate) fn prepare_candidate(
        &self,
        epoch: &str,
        id: &str,
    ) -> Result<Option<CandidatePreparation>> {
        crate::runs::service(&self.connection, epoch, true)?;
        let run = self.run(id)?;
        if run.epoch != epoch
            || run.state != "prepared"
            || run.launch_started
            || !matches!(run.purpose.as_str(), "execute" | "rework")
        {
            return Err(Error::Conflict(
                "候选只可在本服务执行 Run 启动前准备".into(),
            ));
        }
        crate::runs::check_run_authority(&self.connection, &run)?;
        let task = self.task(&run.task_id)?;
        if task.revision != run.task_revision {
            return Err(Error::Conflict("任务依据已变化".into()));
        }
        if candidate(&self.connection, id)?.is_some() {
            return Ok(None);
        }
        let artifact = if run.purpose == "rework" {
            crate::rework::input(&self.connection, &task, &run)?
        } else {
            None
        };
        let input = if artifact.is_none() {
            Some(
                self.input(
                    task.contract
                        .code_input
                        .as_deref()
                        .ok_or_else(|| Error::Conflict("代码执行需要固定输入".into()))?,
                )?,
            )
        } else {
            None
        };
        Ok(Some(CandidatePreparation {
            workspace: self.workspace_path.clone(),
            run,
            input,
            artifact,
            prior_artifact: task.current_artifact,
        }))
    }
    pub(crate) fn publish_candidate(
        &mut self,
        epoch: &str,
        prepared: PreparedCandidate,
    ) -> Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, true)?;
        let run: Run = load(&tx, "runs", &prepared.plan.run.id)?;
        crate::runs::check_run_authority(&tx, &run)?;
        let task: Task = load(&tx, "tasks", &run.task_id)?;
        if run.epoch != epoch
            || run.state != "prepared"
            || run.launch_started
            || run.task_revision != task.revision
            || prepared.plan.run.task_revision != task.revision
            || task.current_artifact != prepared.plan.prior_artifact
            || (prepared
                .plan
                .input
                .as_ref()
                .is_some_and(|i| Some(i.id.as_str()) != task.contract.code_input.as_deref()))
        {
            return Err(Error::Conflict("候选准备期间运行依据变化".into()));
        }
        if candidate(&tx, &run.id)?.is_none() {
            let c = Candidate {
                run_id: run.id.clone(),
                revision: 1,
                files: prepared.files,
            };
            tx.execute(
                "INSERT INTO candidates(run_id,data) VALUES(?1,?2)",
                params![run.id, serde_json::to_string(&c)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}
