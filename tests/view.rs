use atelier::{model::*, store::Store};
use rusqlite::params;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Command as ProcessCommand, Stdio},
    time::Duration,
};

fn sha256(path: &Path) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(fs::read(path).unwrap());
    hasher.finalize().into()
}

fn http(base: &str, path_and_query: &str) -> (u16, String) {
    let host = base
        .trim()
        .trim_start_matches("http://")
        .trim_end_matches('/');
    let mut stream = TcpStream::connect(host).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(
        stream,
        "GET {path_and_query} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, body.to_string())
}

fn rowids(html: &str) -> Vec<i64> {
    let mut values = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("data-rowid=\"") {
        rest = &rest[start + "data-rowid=\"".len()..];
        let end = rest.find('"').unwrap();
        values.push(rest[..end].parse().unwrap());
        rest = &rest[end..];
    }
    values
}

struct ViewProcess {
    child: std::process::Child,
    url: String,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    stdout_rest: std::sync::Arc<std::sync::Mutex<String>>,
    stderr_reader: Option<std::thread::JoinHandle<()>>,
    stdout_reader: Option<std::thread::JoinHandle<()>>,
}

impl ViewProcess {
    fn start(workspace: &Path) -> Self {
        let mut child = ProcessCommand::new(env!("CARGO_BIN_EXE_atelier"))
            .arg("--workspace")
            .arg(workspace)
            .arg("--json")
            .arg("view")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stderr_log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let log = stderr_log.clone();
        let stderr_reader = std::thread::spawn(move || {
            let mut raw = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = std::io::Read::read_to_string(&mut reader, &mut raw);
            *log.lock().unwrap() = raw;
        });
        let stdout_rest = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let stdout_copy = stdout_rest.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let stdout_reader = std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = tx.send(line);
            let mut rest = String::new();
            let _ = std::io::Read::read_to_string(&mut reader, &mut rest);
            *stdout_copy.lock().unwrap() = rest;
        });
        let line = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("view did not print a url");
        let value: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(value["ok"], true, "{line}");
        let url = value["data"]["url"].as_str().unwrap().to_string();
        assert!(url.starts_with("http://127.0.0.1:"), "{url}");
        Self {
            child,
            url,
            stderr: stderr_log,
            stdout_rest,
            stderr_reader: Some(stderr_reader),
            stdout_reader: Some(stdout_reader),
        }
    }
}

impl Drop for ViewProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
        let err = self.stderr.lock().unwrap().clone();
        let rest = self.stdout_rest.lock().unwrap().clone();
        if !err.is_empty() {
            eprintln!("VIEW STDERR:\n{err}");
        }
        if !rest.is_empty() {
            eprintln!("VIEW STDOUT REST:\n{rest}");
        }
    }
}

#[test]
fn missing_workspace_fails_without_creating_it() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent");
    let output = ProcessCommand::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(&missing)
        .arg("view")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!missing.exists());
}

#[test]
fn view_reads_one_workspace_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace");
    let initialized = Store::init(&path, "测试本人").unwrap();
    let human = initialized["self"]["id"].as_str().unwrap().to_string();
    let workspace_id = initialized["id"].as_str().unwrap().to_string();
    let mut store = Store::open(&path).unwrap();
    let mut members = vec![human.clone()];
    for index in 0..3 {
        let worker = store
            .execute(
                &format!("w{index}"),
                &Command::WorkerCreate {
                    connection: None,
                    name: format!("成员 {index}"),
                    description: String::new(),
                },
            )
            .unwrap();
        members.push(worker["id"].as_str().unwrap().to_string());
    }
    let leader = members[1].clone();
    let idle = members[2].clone();
    let team = Team {
        id: String::new(),
        name: "查看团队".into(),
        leader: leader.clone(),
        executor: Some(members[2].clone()),
        verifier: Some(members[3].clone()),
        deployer: None,
        acceptor: human.clone(),
        members: members.clone(),
        grants: BTreeMap::new(),
        revision: 1,
        authorization_revision: 1,
    };
    let team: Team = serde_json::from_value(
        store
            .execute("team", &Command::TeamCreate { team })
            .unwrap(),
    )
    .unwrap();
    let created = store
        .execute(
            "task",
            &Command::TaskCreate {
                team_id: team.id,
                goal: "查看用目标".into(),
                deploy_environment: None,
            },
        )
        .unwrap();
    let task_id = created["task"]["id"].as_str().unwrap().to_string();
    drop(store);

    let db = rusqlite::Connection::open(path.join("atelier.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO messages(id,task_id,sender,recipient,kind,body,reply_to,causation_id,event_key,task_revision,source) VALUES(?1,?2,?3,?4,'work.note',?5,NULL,?6,NULL,1,'worker')",
        params![uuid::Uuid::new_v4().to_string(), task_id, leader, human, "来自负责人", "cause-leader"],
    )
    .unwrap();
    db.execute(
        "INSERT INTO messages(id,task_id,sender,recipient,kind,body,reply_to,causation_id,event_key,task_revision,source) VALUES(?1,?2,NULL,?3,'result',?4,NULL,?5,NULL,1,'core')",
        params![uuid::Uuid::new_v4().to_string(), task_id, human, "来自核心<script>", "cause-core"],
    )
    .unwrap();
    drop(db);

    let database = path.join("atelier.sqlite3");
    let before = sha256(&database);
    let mut entries_before: Vec<_> = fs::read_dir(&path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries_before.sort();
    let view = ViewProcess::start(&path);
    let (status, home) = http(&view.url, "/");
    assert_eq!(status, 200, "{home}");
    assert!(home.contains(&format!("id=\"workspace-id\"><code>{workspace_id}</code>")));
    assert!(home.contains("id=\"runtime-state\">not_started<"));
    assert!(!home.contains("id=\"messages\""));
    assert!(!home.contains("来自核心"));
    assert!(!home.contains("<form"));
    assert!(!home.contains("<button"));
    assert!(!home.contains("credentials"));
    for member in &members {
        assert!(home.contains(&format!("data-worker-id=\"{member}\"")));
    }

    let (status, page) = http(&view.url, &format!("/?worker={human}"));
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("aria-current=\"true\""));
    assert!(page.contains("3 条"));
    assert!(page.contains("查看用目标"));
    assert!(page.contains("来自负责人"));
    assert!(page.contains("来自核心&lt;script&gt;"));
    assert!(page.contains("class=\"party core\">核心</span>"));
    assert!(page.contains(&format!("#latest")));
    assert!(page.contains("id=\"latest\""));
    let ids = rowids(&page);
    assert_eq!(ids.len(), 3, "{page}");
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]), "{ids:?}");
    let latest = page.split("id=\"latest\"").nth(1).unwrap();
    assert!(latest.contains("来自核心&lt;script&gt;"));
    assert!(latest.contains("class=\"party core\">核心</span>"));
    let intake = page
        .split("<li")
        .find(|item| item.contains("查看用目标"))
        .unwrap();
    assert!(
        intake.contains("测试本人"),
        "a core-sourced message with a worker sender must show that worker\n{intake}"
    );
    assert!(!intake.contains("class=\"party core\""));
    assert!(!page.contains("queued"));

    let (status, leader_page) = http(&view.url, &format!("/?worker={leader}"));
    assert_eq!(status, 200);
    assert!(leader_page.contains("2 条"));
    assert!(!leader_page.contains("来自核心"));
    let leader_ids = rowids(&leader_page);
    assert!(leader_ids.windows(2).all(|pair| pair[0] < pair[1]));

    let (status, idle_page) = http(&view.url, &format!("/?worker={idle}"));
    assert_eq!(status, 200, "{idle_page}");
    assert!(idle_page.contains("0 条"));
    assert!(!idle_page.contains("data-message-id"));

    let (status, facts) = http(&view.url, "/facts");
    assert_eq!(status, 200);
    let facts: Value = serde_json::from_str(&facts).unwrap();
    assert_eq!(facts["workspaceId"], workspace_id);
    assert_eq!(facts["runtimeState"], "not_started");
    assert_eq!(facts["workers"].as_array().unwrap().len(), 4);

    let (status, messages) = http(&view.url, &format!("/workers/{human}/messages"));
    assert_eq!(status, 200, "{messages}");
    let messages: Vec<Value> = serde_json::from_str(&messages).unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["sender"]["workerId"], human);
    assert_eq!(messages[0]["sender"]["name"], "测试本人");
    assert!(messages[2]["sender"]["workerId"].is_null());
    assert_eq!(messages[2]["sender"]["name"], "核心");
    assert_eq!(messages[2]["recipient"]["workerId"], human);
    assert_eq!(messages[2]["body"], "来自核心<script>");

    let unknown = uuid::Uuid::new_v4().to_string();
    let (status, body) = http(&view.url, &format!("/?worker={unknown}"));
    assert_eq!(status, 404, "{body}");
    assert!(!body.contains("来自核心"));
    let (status, _) = http(&view.url, "/");
    let (posted, _) = {
        let host = view
            .url
            .trim()
            .trim_start_matches("http://")
            .trim_end_matches('/');
        let mut stream = TcpStream::connect(host).unwrap();
        write!(
            stream,
            "POST / HTTP/1.1\r\nHost: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).unwrap();
        let text = String::from_utf8(raw).unwrap();
        let status: u16 = text.split(' ').nth(1).unwrap().parse().unwrap();
        (status, text)
    };
    assert_eq!(posted, 405);
    assert_eq!(status, 200);

    assert!(!path.join("runtime.lock").exists());
    assert_eq!(sha256(&database), before);
    let mut entries_after: Vec<_> = fs::read_dir(&path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries_after.sort();
    assert_eq!(entries_before, entries_after);

    let status_output = ProcessCommand::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(&path)
        .arg("--json")
        .args(["runtime", "status"])
        .output()
        .unwrap();
    assert!(status_output.status.success(), "{status_output:?}");
    let status_json: Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(status_json["data"]["state"], "not_started");
    drop(view);

    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute(
        "INSERT INTO runtime(singleton,epoch,pid,state,stop_requested) VALUES(1,'epoch',1,'stopped',1)",
        [],
    )
    .unwrap();
    drop(db);
    let stopped = ViewProcess::start(&path);
    let (status, page) = http(&stopped.url, "/");
    assert_eq!(status, 200);
    assert!(page.contains("id=\"runtime-state\">stopped<"));
    let status_output = ProcessCommand::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(&path)
        .arg("--json")
        .args(["runtime", "status"])
        .output()
        .unwrap();
    let status_json: Value = serde_json::from_slice(&status_output.stdout).unwrap();
    assert_eq!(status_json["data"]["state"], "stopped");
}
