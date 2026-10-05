use crate::{Error, Result};
use serde_json::{Value, json};
use std::{fs, io::Write, path::Path, process::Command};

fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(path)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Atelier Sample")
        .env("GIT_COMMITTER_NAME", "Atelier Sample")
        .env("GIT_AUTHOR_EMAIL", "sample@atelier.invalid")
        .env("GIT_COMMITTER_EMAIL", "sample@atelier.invalid")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00+00:00")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00+00:00")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "init.templateDir=",
        ])
        .args(args)
        .output()?;
    if !output.status.success() {
        return Err(Error::Unavailable(format!(
            "样例 Git 准备失败：{}；目标目录保留供检查",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn prepare(path: &Path) -> Result<Value> {
    if path.exists() {
        if fs::symlink_metadata(path)?.file_type().is_symlink()
            || !path.is_dir()
            || fs::read_dir(path)?.next().is_some()
        {
            return Err(Error::Conflict(
                "样例目标须为新目录或空目录，不覆盖任何已有内容".into(),
            ));
        }
    } else {
        fs::create_dir_all(path)?;
    }
    for (name, content) in [
        (
            "index.html",
            include_str!("../samples/tic-tac-toe/index.html"),
        ),
        (
            "README.md",
            include_str!("../samples/tic-tac-toe/README.md"),
        ),
    ] {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join(name))?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
    }
    git(
        path,
        &[
            "init",
            "--quiet",
            "--initial-branch=main",
            "--object-format=sha1",
        ],
    )?;
    git(path, &["add", "--", "index.html", "README.md"])?;
    git(
        path,
        &[
            "commit",
            "--quiet",
            "-m",
            "Atelier tic-tac-toe defect baseline v1",
        ],
    )?;
    let commit = git(path, &["rev-parse", "HEAD"])?;
    Ok(
        json!({"sample":"tic-tac-toe-defect-v1","path":fs::canonicalize(path)?,"commit":commit,"knownDefect":"两条对角线漏判","status":"prepared","taskCreated":false}),
    )
}
