//! Registered host deploy targets. A registration describes where a deploy
//! lands and how the core checks it. It does not grant permission.
use crate::{
    Error, Result,
    model::*,
    store::{self, load, new_id, text},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const AUTO_APPROVAL_WARNING: &str = "auto approval runs host commands as the local user without a sandbox; the command can reach the Atelier CLI and the workspace";

pub(crate) fn create(
    db: &Connection,
    actor: &str,
    workspace: &Path,
    command: &Command,
) -> Result<Value> {
    let Command::EnvironmentCreate {
        name,
        code_root,
        port,
        health_path,
        approval,
        verify_timeout,
        verify_argv,
    } = command
    else {
        return Err(Error::Invalid("环境登记命令无效".into()));
    };
    require_self(db, actor)?;
    let environment = Environment {
        id: new_id(),
        name: name.clone(),
        code_root: code_root.clone(),
        service: service(*port, health_path)?,
        verification: verification(verify_argv, *verify_timeout)?,
        approval: approval.clone(),
        revision: 1,
    };
    validate(db, workspace, &environment, None)?;
    save(db, &environment)?;
    Ok(view(&environment))
}

pub(crate) fn update(
    db: &Connection,
    actor: &str,
    workspace: &Path,
    command: &Command,
) -> Result<Value> {
    let Command::EnvironmentUpdate {
        name,
        revision,
        code_root,
        port,
        health_path,
        no_service,
        approval,
        verify_timeout,
        verify_files,
        verify_argv,
    } = command
    else {
        return Err(Error::Invalid("环境登记命令无效".into()));
    };
    require_self(db, actor)?;
    if *no_service && (port.is_some() || health_path.is_some()) {
        return Err(Error::Invalid(
            "清除服务时不能同时填写端口或健康检查路径".into(),
        ));
    }
    if *verify_files && !verify_argv.is_empty() {
        return Err(Error::Invalid("改回文件比对时不能同时填写核对命令".into()));
    }
    let mut environment = load_named(db, name)?;
    store::revision(environment.revision, *revision)?;
    if let Some(root) = code_root {
        environment.code_root = root.clone();
    }
    if *no_service {
        environment.service = None;
    } else if port.is_some() || health_path.is_some() {
        let next_port = (*port).or(environment.service.as_ref().map(|service| service.port));
        let next_health = health_path.clone().or_else(|| {
            environment
                .service
                .as_ref()
                .map(|service| service.health_path.clone())
        });
        environment.service = service(next_port, &next_health)?;
    }
    if *verify_files {
        environment.verification = VerifyMethod::Files;
    } else if !verify_argv.is_empty() {
        environment.verification = verification(verify_argv, *verify_timeout)?;
    } else if let (VerifyMethod::Command { argv, .. }, Some(timeout)) =
        (&environment.verification, verify_timeout)
    {
        environment.verification = verification(argv, Some(*timeout))?;
    }
    if let Some(approval) = approval {
        environment.approval = approval.clone();
    }
    environment.revision += 1;
    validate(db, workspace, &environment, Some(&environment.id))?;
    save(db, &environment)?;
    Ok(view(&environment))
}

pub(crate) fn list(db: &Connection) -> Result<Value> {
    let mut statement =
        db.prepare("SELECT data FROM environments ORDER BY json_extract(data,'$.name')")?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let environments = rows
        .iter()
        .map(|row| serde_json::from_str::<Environment>(row))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(json!(environments.iter().map(view).collect::<Vec<_>>()))
}

pub(crate) fn show(db: &Connection, name: &str) -> Result<Value> {
    Ok(view(&load_named(db, name)?))
}

pub(crate) fn load_named(db: &Connection, name: &str) -> Result<Environment> {
    let row: Option<String> = db
        .query_row(
            "SELECT data FROM environments WHERE json_extract(data,'$.name')=?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    let row = row.ok_or_else(|| Error::NotFound(format!("环境不存在：{name}")))?;
    Ok(serde_json::from_str(&row)?)
}

pub(crate) fn require_current(db: &Connection, snapshot: &Environment) -> Result<Environment> {
    let current = load_named(db, &snapshot.name)?;
    if !current.same_registration(snapshot) {
        return Err(Error::Conflict("部署目标登记已变化".into()));
    }
    Ok(current)
}

fn require_self(db: &Connection, actor: &str) -> Result<()> {
    let self_id: String = db.query_row("SELECT self_id FROM workspace", [], |row| row.get(0))?;
    if actor != self_id {
        return Err(Error::Forbidden("只有本机本人可以登记环境".into()));
    }
    Ok(())
}

fn service(port: Option<u16>, health_path: &Option<String>) -> Result<Option<HostService>> {
    match (port, health_path) {
        (None, None) => Ok(None),
        (Some(port), Some(path)) => Ok(Some(HostService {
            port,
            health_path: path.clone(),
        })),
        _ => Err(Error::Invalid("服务端口和健康检查路径必须同时填写".into())),
    }
}

fn verification(argv: &[String], timeout: Option<u32>) -> Result<VerifyMethod> {
    if argv.is_empty() {
        if timeout.is_some() {
            return Err(Error::Invalid("文件比对不能填写核对超时".into()));
        }
        return Ok(VerifyMethod::Files);
    }
    Ok(VerifyMethod::Command {
        argv: argv.to_vec(),
        timeout_seconds: timeout.unwrap_or(120),
    })
}

fn validate(
    db: &Connection,
    workspace: &Path,
    environment: &Environment,
    ignore_id: Option<&str>,
) -> Result<()> {
    valid_name(&environment.name)?;
    if name_taken(db, &environment.name, ignore_id)? {
        return Err(Error::Invalid("环境名重复".into()));
    }
    let root = canonical_root(&environment.code_root)?;
    let home = home_dir()?;
    if root == home || home.starts_with(&root) {
        return Err(Error::Invalid("代码目录不能是家目录或其上级".into()));
    }
    let workspace = fs::canonicalize(workspace)?;
    if root.starts_with(&workspace) || workspace.starts_with(&root) {
        return Err(Error::Invalid(
            "代码目录不能位于工作区内或包含工作区".into(),
        ));
    }
    for relative in [".ssh", ".gnupg", "Library/Keychains", ".config/gh"] {
        let sensitive = home.join(relative);
        if root.starts_with(&sensitive) || sensitive.starts_with(&root) {
            return Err(Error::Invalid(format!(
                "代码目录不能触及登录材料目录 {}",
                sensitive.display()
            )));
        }
    }
    let mut statement = db.prepare("SELECT data FROM environments")?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for row in rows {
        let other: Environment = serde_json::from_str(&row)?;
        if Some(other.id.as_str()) == ignore_id {
            continue;
        }
        let other_root = PathBuf::from(&other.code_root);
        if root == other_root || root.starts_with(&other_root) || other_root.starts_with(&root) {
            return Err(Error::Invalid(format!(
                "代码目录与环境 {} 重叠",
                other.name
            )));
        }
        if let (Some(service), Some(other_service)) = (&environment.service, &other.service) {
            if service.port == other_service.port {
                return Err(Error::Invalid(format!(
                    "端口 {} 已被环境 {} 登记",
                    service.port, other.name
                )));
            }
        }
    }
    if let Some(service) = &environment.service {
        if service.port == 0 {
            return Err(Error::Invalid("端口不能为 0".into()));
        }
        if !service.health_path.starts_with('/')
            || service.health_path.len() > 1024
            || !service
                .health_path
                .bytes()
                .all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(Error::Invalid(
                "健康检查路径必须以 / 开头，且只含可见 ASCII，最长 1024 字节".into(),
            ));
        }
    }
    if let VerifyMethod::Command {
        argv,
        timeout_seconds,
    } = &environment.verification
    {
        validate_argv(argv)?;
        if !(1..=600).contains(timeout_seconds) {
            return Err(Error::Invalid("核对命令超时须在 1 到 600 秒之间".into()));
        }
    }
    Ok(())
}

pub(crate) fn validate_argv(argv: &[String]) -> Result<()> {
    if argv.is_empty() || argv.len() > 64 {
        return Err(Error::Invalid("命令参数须为 1 到 64 项".into()));
    }
    if !Path::new(&argv[0]).is_absolute() {
        return Err(Error::Invalid("命令的第一个参数必须是绝对路径".into()));
    }
    for item in argv {
        if item.is_empty() || item.len() > 65_536 || item.contains('\0') {
            return Err(Error::Invalid(
                "命令参数不能为空、不能含 NUL，且每项最长 64 KiB".into(),
            ));
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> Result<()> {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err(Error::Invalid("环境名无效".into()));
    };
    if name.len() > 63
        || !first.is_ascii_lowercase() && !first.is_ascii_digit()
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(Error::Invalid(
            "环境名须匹配 ^[a-z0-9][a-z0-9-]{0,62}$".into(),
        ));
    }
    text(name, "环境名", 63)?;
    Ok(())
}

fn name_taken(db: &Connection, name: &str, ignore_id: Option<&str>) -> Result<bool> {
    let existing: Option<String> = db
        .query_row(
            "SELECT id FROM environments WHERE json_extract(data,'$.name')=?1",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    Ok(existing.is_some_and(|id| Some(id.as_str()) != ignore_id))
}

fn canonical_root(input: &str) -> Result<PathBuf> {
    let path = Path::new(input);
    if !path.is_absolute() {
        return Err(Error::Invalid("代码目录必须是绝对路径".into()));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::Invalid("代码目录不存在".into())
        } else {
            Error::Io(error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::Invalid(
            "代码目录必须是不含符号链接的已存在目录".into(),
        ));
    }
    let canonical = fs::canonicalize(path)?;
    if canonical != path {
        return Err(Error::Invalid(
            "代码目录必须是已解析的规范绝对路径，不能含符号链接".into(),
        ));
    }
    Ok(canonical)
}

fn home_dir() -> Result<PathBuf> {
    let home =
        std::env::var("HOME").map_err(|_| Error::Invalid("无法读取 HOME，不能登记环境".into()))?;
    fs::canonicalize(home).map_err(Error::Io)
}

fn save(db: &Connection, environment: &Environment) -> Result<()> {
    db.execute(
        "INSERT INTO environments(id,data) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
        params![environment.id, serde_json::to_string(environment)?],
    )?;
    Ok(())
}

fn view(environment: &Environment) -> Value {
    let mut value = json!(environment);
    if environment.approval == CommandApproval::Auto {
        value["warning"] = json!(AUTO_APPROVAL_WARNING);
    }
    value
}

pub(crate) fn unchanged(db: &Connection, snapshot: &Environment) -> Result<bool> {
    match load::<Environment>(db, "environments", &snapshot.id) {
        Ok(current) => Ok(current.same_registration(snapshot)),
        Err(Error::NotFound(_)) => Ok(false),
        Err(error) => Err(error),
    }
}
