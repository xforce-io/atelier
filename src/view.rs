//! Read-only loopback view of one workspace. It never opens a writable
//! database connection, never creates `runtime.lock`, and never starts the
//! workspace runtime.

use crate::{Error, Result};
use fs2::FileExt;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::Duration,
};

const DATABASE: &str = "atelier.sqlite3";

#[derive(Clone, Serialize)]
struct WorkerView {
    id: String,
    name: String,
}

#[derive(Clone, Serialize)]
struct PartyView {
    #[serde(rename = "workerId")]
    worker_id: Option<String>,
    name: String,
}

#[derive(Clone, Serialize)]
struct MessageView {
    id: String,
    rowid: i64,
    sender: PartyView,
    recipient: PartyView,
    body: String,
}

struct Snapshot {
    workspace_id: String,
    runtime_state: String,
    workers: Vec<WorkerView>,
    selected: Option<String>,
    messages: Option<Vec<MessageView>>,
}

pub fn serve(workspace: &Path, json_output: bool) -> Result<()> {
    let workspace = open_readonly(workspace)?.1;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/");
    if json_output {
        println!("{}", json!({"version":2,"ok":true,"data":{"url":url}}));
    } else {
        println!("{url}");
    }
    let _ = std::io::stdout().flush();
    for incoming in listener.incoming() {
        let mut stream = match incoming {
            Ok(stream) => stream,
            Err(error) => return Err(error.into()),
        };
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        if let Err(error) = respond(&mut stream, &workspace) {
            let _ = write_http(
                &mut stream,
                error_status(&error),
                error_reason(&error),
                "text/plain; charset=utf-8",
                error.to_string().as_bytes(),
            );
        }
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> Result<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffer.len() > 8192 {
            return Err(Error::Invalid("request header is too large".into()));
        }
        let read = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        };
        buffer.extend_from_slice(&chunk[..read]);
    }
    if buffer.is_empty() {
        return Err(Error::Invalid("empty request".into()));
    }
    String::from_utf8(buffer).map_err(|_| Error::Invalid("request is not UTF-8".into()))
}

fn respond(stream: &mut TcpStream, workspace: &Path) -> Result<()> {
    let request = read_request(stream)?;
    let line = request
        .lines()
        .next()
        .ok_or_else(|| Error::Invalid("missing request line".into()))?;
    let mut parts = line.split(' ');
    let method = parts
        .next()
        .ok_or_else(|| Error::Invalid("missing method".into()))?;
    let target = parts
        .next()
        .ok_or_else(|| Error::Invalid("missing request path".into()))?;
    if method != "GET" {
        write_http(
            stream,
            405,
            "Method Not Allowed",
            "text/plain; charset=utf-8",
            b"GET only",
        )?;
        return Ok(());
    }
    let (path, query) = target
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((target, None));
    match path {
        "/" => {
            let selected = parse_worker_query(query)?;
            let snapshot = load_snapshot(workspace, selected.as_deref())?;
            let html = render_page(&snapshot);
            write_http(
                stream,
                200,
                "OK",
                "text/html; charset=utf-8",
                html.as_bytes(),
            )?;
        }
        "/facts" => {
            if query.is_some() {
                return Err(Error::Invalid("unknown query".into()));
            }
            let snapshot = load_snapshot(workspace, None)?;
            let body = serde_json::to_vec(&json!({
                "workspaceId": snapshot.workspace_id,
                "runtimeState": snapshot.runtime_state,
                "workers": snapshot.workers,
            }))?;
            write_http(stream, 200, "OK", "application/json", &body)?;
        }
        path if path.starts_with("/workers/") && path.ends_with("/messages") => {
            if query.is_some() {
                return Err(Error::Invalid("unknown query".into()));
            }
            let raw_id = path
                .strip_prefix("/workers/")
                .and_then(|rest| rest.strip_suffix("/messages"))
                .ok_or_else(|| Error::NotFound("unknown path".into()))?;
            let id = parse_worker_id(raw_id)?;
            let snapshot = load_snapshot(workspace, Some(&id))?;
            let body = serde_json::to_vec(&snapshot.messages.unwrap_or_default())?;
            write_http(stream, 200, "OK", "application/json", &body)?;
        }
        _ => return Err(Error::NotFound("unknown path".into())),
    }
    Ok(())
}

fn parse_worker_query(query: Option<&str>) -> Result<Option<String>> {
    let Some(query) = query.filter(|query| !query.is_empty()) else {
        return Ok(None);
    };
    let mut selected = None;
    for pair in query.split('&') {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| Error::Invalid("invalid query".into()))?;
        if key != "worker" || selected.is_some() {
            return Err(Error::Invalid("invalid query".into()));
        }
        selected = Some(parse_worker_id(value)?);
    }
    Ok(selected)
}

fn parse_worker_id(value: &str) -> Result<String> {
    let id = uuid::Uuid::parse_str(value)
        .map_err(|_| Error::Invalid("worker id must be a UUID".into()))?;
    Ok(id.to_string())
}

fn open_readonly(path: &Path) -> Result<(Connection, PathBuf)> {
    let workspace = fs::canonicalize(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound("工作区不存在；请先 workspace init".into())
        } else {
            Error::Io(error)
        }
    })?;
    let file = workspace.join(DATABASE);
    let metadata = fs::symlink_metadata(&file).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound("工作区不存在；请先 workspace init".into())
        } else {
            Error::Io(error)
        }
    })?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::Invalid("数据库必须是工作区中的普通文件".into()));
    }
    let db = Connection::open_with_flags(
        &file,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_secs(2))?;
    db.pragma_update(None, "query_only", true)?;
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != 24 {
        return Err(Error::Invalid(format!(
            "工作区格式不支持或初始化未完成：{version}；请保留原目录修复"
        )));
    }
    Ok((db, workspace))
}

fn load_snapshot(workspace: &Path, selected: Option<&str>) -> Result<Snapshot> {
    let (db, workspace) = open_readonly(workspace)?;
    let workspace_id: String = db.query_row("SELECT id FROM workspace", [], |row| row.get(0))?;
    let runtime_state = runtime_state(&workspace, &db)?;
    let mut stmt = db.prepare("SELECT data FROM workers ORDER BY rowid")?;
    let workers = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .map(|row| {
            let worker: crate::model::Worker = serde_json::from_str(&row?)?;
            Ok(WorkerView {
                id: worker.id,
                name: worker.name,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    drop(stmt);
    let messages = match selected {
        None => None,
        Some(id) => {
            if !workers.iter().any(|worker| worker.id == id) {
                return Err(Error::NotFound("worker does not exist".into()));
            }
            Some(messages_for(&db, &workers, id)?)
        }
    };
    Ok(Snapshot {
        workspace_id,
        runtime_state,
        workers,
        selected: selected.map(str::to_string),
        messages,
    })
}

fn messages_for(
    db: &Connection,
    workers: &[WorkerView],
    worker_id: &str,
) -> Result<Vec<MessageView>> {
    let mut stmt = db.prepare(
        "SELECT id, sender, recipient, body, rowid FROM messages WHERE sender = ?1 OR recipient = ?1 ORDER BY rowid ASC",
    )?;
    let rows = stmt
        .query_map([worker_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, sender, recipient, body, rowid)| {
            Ok(MessageView {
                id,
                rowid,
                sender: party(workers, sender)?,
                recipient: party(workers, Some(recipient))?,
                body,
            })
        })
        .collect()
}

fn party(workers: &[WorkerView], worker_id: Option<String>) -> Result<PartyView> {
    let Some(worker_id) = worker_id else {
        return Ok(PartyView {
            worker_id: None,
            name: "核心".to_string(),
        });
    };
    let worker = workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .ok_or_else(|| Error::Invalid("message party is not a worker".into()))?;
    Ok(PartyView {
        worker_id: Some(worker.id.clone()),
        name: worker.name.clone(),
    })
}

fn runtime_state(workspace: &Path, db: &Connection) -> Result<String> {
    let record: Option<(String, bool)> = db
        .query_row(
            "SELECT state, stop_requested FROM runtime WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let (state, stop_requested) = record.unwrap_or_else(|| ("not_started".to_string(), false));
    Ok(overlay_state(&state, stop_requested, lock_held(workspace)?))
}

fn overlay_state(state: &str, stop_requested: bool, held: bool) -> String {
    if held && state == "not_started" {
        "starting".to_string()
    } else if held && (stop_requested || state == "stopped") {
        "stopping".to_string()
    } else if !held && state == "running" {
        "interrupted".to_string()
    } else {
        state.to_string()
    }
}

fn lock_held(workspace: &Path) -> Result<bool> {
    let file_path = workspace.join("runtime.lock");
    match fs::symlink_metadata(&file_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(Error::Invalid("服务锁必须为普通文件".into()));
        }
        Ok(_) => {}
    }
    let file = OpenOptions::new().read(true).open(file_path)?;
    match FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn render_page(snapshot: &Snapshot) -> String {
    let mut workers = String::new();
    for worker in &snapshot.workers {
        let current = snapshot.selected.as_deref() == Some(worker.id.as_str());
        let aria = if current {
            " aria-current=\"true\""
        } else {
            ""
        };
        workers.push_str(&format!(
            "<li><a class=\"worker\" href=\"/?worker={}#latest\" data-worker-id=\"{}\"{aria}><span class=\"name\">{}</span> <code>{}</code></a></li>",
            escape(&worker.id),
            escape(&worker.id),
            escape(&worker.name),
            escape(&worker.id),
        ));
    }
    let messages = match &snapshot.messages {
        None => {
            "<p id=\"messages-empty\">点一名工作成员后查看其发出和收到的工作消息。</p>".to_string()
        }
        Some(messages) if messages.is_empty() => {
            "<p id=\"messages-count\">0 条</p><ol id=\"messages\"></ol>".to_string()
        }
        Some(messages) => {
            let mut list = format!(
                "<p id=\"messages-count\">{} 条</p><ol id=\"messages\">",
                messages.len()
            );
            for (index, message) in messages.iter().enumerate() {
                let latest = index + 1 == messages.len();
                let id_attr = if latest { " id=\"latest\"" } else { "" };
                list.push_str(&format!(
                    "<li{id_attr} data-message-id=\"{}\" data-rowid=\"{}\"><p>发送者：{}</p><p>接收者：{}</p><pre class=\"body\">{}</pre></li>",
                    escape(&message.id),
                    message.rowid,
                    party_html(&message.sender),
                    party_html(&message.recipient),
                    escape(&message.body),
                ));
            }
            list.push_str("</ol><script>document.getElementById(\"latest\")?.scrollIntoView({block:\"end\"})</script>");
            list
        }
    };
    format!(
        "<!DOCTYPE html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><title>只读工作区</title><style>{style}</style></head><body><header><p>工作区</p><p id=\"workspace-id\"><code>{workspace}</code></p><p>运行状态</p><p id=\"runtime-state\">{state}</p></header><main><section><h1>工作成员</h1><ul id=\"workers\">{workers}</ul></section><section><h1>工作消息</h1>{messages}</section></main></body></html>",
        style = PAGE_STYLE,
        workspace = escape(&snapshot.workspace_id),
        state = escape(&snapshot.runtime_state),
        workers = workers,
        messages = messages,
    )
}

fn party_html(party: &PartyView) -> String {
    match &party.worker_id {
        None => format!("<span class=\"party core\">{}</span>", escape(&party.name)),
        Some(id) => format!(
            "<span class=\"party\" data-worker-id=\"{}\">{} <code>{}</code></span>",
            escape(id),
            escape(&party.name),
            escape(id),
        ),
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn write_http(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}

fn error_status(error: &Error) -> u16 {
    match error {
        Error::Invalid(_) => 400,
        Error::NotFound(_) => 404,
        Error::Unavailable(_) => 503,
        _ => 500,
    }
}

fn error_reason(error: &Error) -> &'static str {
    match error_status(error) {
        400 => "Bad Request",
        404 => "Not Found",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

const PAGE_STYLE: &str = "body{margin:0;background:#f3efe6;color:#1d1a16;font:16px/1.5 'Iowan Old Style',Palatino,'Songti SC',serif}header,main{padding:24px}main{display:grid;grid-template-columns:16rem 1fr;gap:24px}a{color:inherit}#workers{list-style:none;padding:0}a.worker{display:block;padding:8px 10px;text-decoration:none}a.worker[aria-current]{background:#1d1a16;color:#f3efe6}#messages{max-height:70vh;overflow:auto;padding-left:1.2rem}pre.body{white-space:pre-wrap;font:inherit;margin:0}code{font-family:ui-monospace,monospace;font-size:.85em}";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn overlay_matches_runtime_status_rules() {
        assert_eq!(overlay_state("not_started", false, true), "starting");
        assert_eq!(overlay_state("running", true, true), "stopping");
        assert_eq!(overlay_state("stopped", false, true), "stopping");
        assert_eq!(overlay_state("stopped", true, false), "stopped");
        assert_eq!(overlay_state("running", false, false), "interrupted");
        assert_eq!(
            overlay_state("blocked_unknown", false, false),
            "blocked_unknown"
        );
    }

    #[test]
    fn readonly_open_rejects_writes_and_does_not_create_lock() {
        let dir = tempfile::tempdir().unwrap();
        Store::init(dir.path(), "本人").unwrap();
        let (db, workspace) = open_readonly(dir.path()).unwrap();
        let error = db.execute("UPDATE workspace SET id = id", []).unwrap_err();
        assert!(
            error.to_string().contains("readonly") || format!("{error:?}").contains("ReadOnly"),
            "{error}"
        );
        assert!(!workspace.join("runtime.lock").exists());
        let snapshot = load_snapshot(&workspace, None).unwrap();
        assert_eq!(snapshot.runtime_state, "not_started");
        assert!(snapshot.messages.is_none());
        assert!(!workspace.join("runtime.lock").exists());
    }

    #[test]
    fn unsupported_schema_fails_without_migration() {
        let dir = tempfile::tempdir().unwrap();
        Store::init(dir.path(), "本人").unwrap();
        let file = dir.path().join(DATABASE);
        let db = Connection::open(&file).unwrap();
        db.pragma_update(None, "user_version", 22).unwrap();
        drop(db);
        let before = fs::read(&file).unwrap();
        let error = open_readonly(dir.path()).unwrap_err();
        assert!(error.to_string().contains("22"), "{error}");
        assert_eq!(fs::read(&file).unwrap(), before);
    }

    #[test]
    fn page_escapes_text_and_marks_core_sender() {
        let party = PartyView {
            worker_id: None,
            name: "核心".to_string(),
        };
        assert!(party_html(&party).contains("核心"));
        assert!(!party_html(&party).contains("data-worker-id"));
        assert_eq!(escape("<script>\"&"), "&lt;script&gt;&quot;&amp;");
    }
}
