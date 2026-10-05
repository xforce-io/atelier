//! Fixed Git inputs and content-addressed blobs. Never checks out candidate code
//! or invokes its hooks, filters, submodules or scripts.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub const INPUT_LIMIT: u64 = 50 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitInput {
    pub id: String,
    pub repository: String,
    pub commit: String,
    pub content_digest: String,
    pub total_bytes: u64,
    pub files: Vec<FileEntry>,
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains('\\')
        || path.contains('\0')
        || path.split('/').any(|p| {
            p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".git") || p.len() > 255
        })
        || !Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
    {
        return Err(Error::Invalid("输入包含不安全或不支持的相对路径".into()));
    }
    Ok(())
}

fn git(repository: &Path, args: &[&str], limit: u64) -> Result<Vec<u8>> {
    let mut child = Command::new("git")
        .current_dir(repository)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args([
            "--no-pager",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "protocol.allow=never",
        ])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Unavailable("Git 输出管道不可用".into()))?;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(error.into());
            }
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(Error::Unavailable("读取固定 Git 对象超时".into()));
        }
        thread::sleep(Duration::from_millis(2));
    };
    let bytes = reader
        .join()
        .map_err(|_| Error::Unavailable("Git 输出读取失败".into()))??;
    if bytes.len() as u64 > limit {
        return Err(Error::Invalid("Git 输入超出大小限制".into()));
    }
    if !status.success() {
        return Err(Error::Invalid(
            "无法读取指定 Git commit/对象；不自动获取远程对象".into(),
        ));
    }
    Ok(bytes)
}

fn object_dir(workspace: &Path) -> Result<std::path::PathBuf> {
    let mut path = workspace.to_path_buf();
    for name in ["objects", "blobs"] {
        path.push(name);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let meta = fs::symlink_metadata(&path)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::Invalid(
                "核心对象目录不能为符号链接或其他文件".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        }
        // Persist the directory entry before any database reference is published.
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
    }
    Ok(path)
}

pub(crate) fn store_blob(workspace: &Path, bytes: &[u8]) -> Result<String> {
    let hash = digest(bytes);
    let dir = object_dir(workspace)?;
    let destination = dir.join(&hash);
    let temporary = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&destination)?;
                if !metadata.is_file()
                    || metadata.file_type().is_symlink()
                    || metadata.len() != bytes.len() as u64
                    || digest(&fs::read(&destination)?) != hash
                {
                    return Err(Error::Conflict("已有固定内容损坏，不覆盖历史对象".into()));
                }
            }
            Err(error) => return Err(error.into()),
        }
        fs::File::open(&dir)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result?;
    Ok(hash)
}

pub fn import_git(workspace: &Path, repository: &Path, commit: &str) -> Result<GitInput> {
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Invalid(
            "输入必须指定完整 Git commit SHA，不能使用可变分支或标签".into(),
        ));
    }
    let repository = fs::canonicalize(repository)?;
    if git(&repository, &["cat-file", "-t", commit], 64)? != b"commit\n" {
        return Err(Error::Invalid("输入引用必须是 commit 对象".into()));
    }
    let listing = git(
        &repository,
        &["ls-tree", "-r", "-z", "--full-tree", commit],
        10 * 1024 * 1024,
    )?;
    let mut files = Vec::new();
    let mut total_bytes = 0;
    let deadline = Instant::now() + Duration::from_secs(120);
    for record in listing.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if Instant::now() >= deadline {
            return Err(Error::Unavailable("导入固定输入超时".into()));
        }
        if files.len() >= 10000 {
            return Err(Error::Invalid("输入文件数超过 10000".into()));
        }
        let record = std::str::from_utf8(record)
            .map_err(|_| Error::Invalid("输入文件名必须为 UTF-8".into()))?;
        let (header, path) = record
            .split_once('\t')
            .ok_or_else(|| Error::Invalid("Git tree 格式错误".into()))?;
        let parts: Vec<_> = header.split(' ').collect();
        if parts.len() != 3 || parts[1] != "blob" || !matches!(parts[0], "100644" | "100755") {
            return Err(Error::Invalid("不接受符号链接、子模块或特殊文件".into()));
        }
        relative_path(path)?;
        let bytes = git(
            &repository,
            &["cat-file", "blob", parts[2]],
            INPUT_LIMIT - total_bytes,
        )?;
        total_bytes += bytes.len() as u64;
        let sha256 = store_blob(workspace, &bytes)?;
        files.push(FileEntry {
            path: path.into(),
            sha256,
            size: bytes.len() as u64,
            executable: parts[0] == "100755",
        });
    }
    object_dir(workspace)?;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let content_digest = digest(&serde_json::to_vec(&files)?);
    let repository = repository
        .to_str()
        .ok_or_else(|| Error::Invalid("输入目录须为 UTF-8".into()))?
        .to_string();
    let commit = commit.to_ascii_lowercase();
    let id = digest(&serde_json::to_vec(&(
        &repository,
        &commit,
        &content_digest,
    ))?);
    Ok(GitInput {
        id,
        repository,
        commit,
        content_digest,
        total_bytes,
        files,
    })
}

pub fn validate_input(workspace: &Path, input: &GitInput) -> Result<()> {
    if input.files.len() > 10000
        || input.total_bytes > INPUT_LIMIT
        || input
            .files
            .iter()
            .try_fold(0u64, |total, file| total.checked_add(file.size))
            != Some(input.total_bytes)
        || input
            .files
            .windows(2)
            .any(|pair| pair[0].path >= pair[1].path)
        || digest(&serde_json::to_vec(&(
            &input.repository,
            &input.commit,
            &input.content_digest,
        ))?) != input.id
        || digest(&serde_json::to_vec(&input.files)?) != input.content_digest
    {
        return Err(Error::Conflict("输入清单摘要不匹配".into()));
    }
    for name in ["objects", "objects/blobs"] {
        let metadata = fs::symlink_metadata(workspace.join(name))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Conflict("固定内容目录无效".into()));
        }
    }
    for file in &input.files {
        relative_path(&file.path)?;
        if file.sha256.len() != 64 || !file.sha256.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(Error::Invalid("内容摘要无效".into()));
        }
        let path = workspace.join("objects/blobs").join(&file.sha256);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() != file.size
            || file.size > INPUT_LIMIT
            || digest(&fs::read(&path)?) != file.sha256
        {
            return Err(Error::Conflict("固定输入内容丢失或损坏".into()));
        }
    }
    Ok(())
}
