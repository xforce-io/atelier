use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
};

fn invoke(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(path)
        .arg("--json")
        .args(args)
        .output()
        .unwrap()
}
fn success(path: &Path, args: &[&str]) -> Value {
    let output = invoke(path, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["version"], 2);
    assert_eq!(value["ok"], true);
    value["data"].clone()
}

#[test]
#[ignore = "requires an explicitly prepared immutable Atelier CLI image; never uses native login"]
fn real_cli_prepare_is_private_worker_bound_persistent_and_distinct_from_login() {
    let image = std::env::var("ATELIER_CLI_IMAGE").expect("set the prepared image digest");
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("workspace");
    success(&p, &["workspace", "init", "--name", "CLI 环境测试"]);
    let config = dir.path().join("cli.json");
    std::fs::write(&config,serde_json::json!({"transport":"agent-cli","runtime":"pi","image":image,"egress_hosts":["example.com"]}).to_string()).unwrap();
    let connection = success(
        &p,
        &[
            "--request-id",
            "connection",
            "connection",
            "create",
            "--name",
            "CLI",
            "--file",
            config.to_str().unwrap(),
        ],
    );
    let id = connection["connection"]["id"].as_str().unwrap();
    let worker = success(
        &p,
        &[
            "--request-id",
            "worker",
            "worker",
            "create",
            "--name",
            "专用成员",
            "--connection",
            id,
        ],
    );
    let worker_id = worker["id"].as_str().unwrap();
    let args = [
        "--request-id",
        "prepare",
        "connection",
        "prepare",
        id,
        "--revision",
        "1",
        "--worker",
        worker_id,
    ];
    let result = success(&p, &args);
    assert_eq!(result["environment"]["state"], "prepared", "{result}");
    assert_eq!(result["environment"]["code"], "prepared_not_authenticated");
    assert_eq!(result["environment"]["loginGeneration"], 0);
    assert_eq!(result["environment"]["workerId"], worker_id);
    assert_eq!(success(&p, &args), result);
    assert_eq!(success(&p, &["request", "show", "prepare"]), result);
    let environment = worker["execution_config"].as_str().unwrap();
    let root = p.join("cli-environments").join(environment);
    assert_eq!(std::fs::read_dir(root.join("login")).unwrap().count(), 0);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root.join("login"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    let other = success(
        &p,
        &[
            "--request-id",
            "other-worker",
            "worker",
            "create",
            "--name",
            "另一成员",
            "--connection",
            id,
        ],
    );
    let separate = success(
        &p,
        &[
            "--request-id",
            "other-prepare",
            "connection",
            "prepare",
            id,
            "--revision",
            "1",
            "--worker",
            other["id"].as_str().unwrap(),
        ],
    );
    assert_ne!(separate["environment"]["id"], result["environment"]["id"]);
    let shown = success(&p, &["connection", "show", id]);
    assert_eq!(shown["cliEnvironments"].as_array().unwrap().len(), 2);
    assert_eq!(shown["executionSupported"], true);
    assert_eq!(success(&p, &["task", "list"]), serde_json::json!([]));
    let sql = rusqlite::Connection::open(p.join("atelier.sqlite3")).unwrap();
    assert_eq!(
        sql.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    std::fs::remove_dir(root.join("login")).unwrap();
    let failed = success(
        &p,
        &[
            "--request-id",
            "missing-storage",
            "connection",
            "prepare",
            id,
            "--revision",
            "1",
            "--worker",
            worker_id,
        ],
    );
    assert_eq!(failed["environment"]["state"], "failed");
    assert_eq!(failed["environment"]["code"], "private_environment_invalid");
    assert!(!root.join("login").exists());
    let evidence = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".agents/verify-runs/1")
        .join(format!("cli-prepare-{}.json", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(evidence.parent().unwrap()).unwrap();
    std::fs::write(&evidence,serde_json::json!({"kind":"development_integration","image":image,"prepared":result,"separateWorker":separate,"missingStorage":failed,"nativeLogin":false,"model":false}).to_string()).unwrap();
    println!("evidence: {}", evidence.display());
}

#[test]
fn separate_cli_processes_persist_team_task_and_mailbox() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("中文 空格工作区");
    let initialized = success(&p, &["workspace", "init", "--name", "测试本人"]);
    let human = initialized["self"]["id"].as_str().unwrap();
    let worker = success(
        &p,
        &[
            "--request-id",
            "w",
            "worker",
            "create",
            "--name",
            "数字团队负责人",
        ],
    );
    let leader = worker["id"].as_str().unwrap();
    let members = format!("{human},{leader}");
    let team = success(
        &p,
        &[
            "--request-id",
            "team",
            "team",
            "create",
            "--name",
            "测试团队",
            "--members",
            &members,
            "--leader",
            leader,
        ],
    );
    let team_id = team["id"].as_str().unwrap();
    let args = [
        "--request-id",
        "task",
        "task",
        "create",
        "--team",
        team_id,
        "--goal",
        "调查 \"带引号\"\n多行目标",
    ];
    let task = success(&p, &args);
    assert_eq!(task["task"]["state"], "pending");
    assert_eq!(task["delivery"]["status"], "queued");
    assert_eq!(success(&p, &args), task);
    let queue = success(&p, &["mailbox", "list", "--worker", leader]);
    assert_eq!(queue.as_array().unwrap().len(), 1);
    assert_eq!(queue[0]["message"]["body"], "调查 \"带引号\"\n多行目标");
    let id = task["task"]["id"].as_str().unwrap();
    let denied = invoke(
        &p,
        &[
            "--request-id",
            "fake",
            "task",
            "intake",
            id,
            "--revision",
            "1",
            "--decision",
            "wait",
            "--reason",
            "冒充",
        ],
    );
    assert!(!denied.status.success());
    let error: Value = serde_json::from_slice(&denied.stdout).unwrap();
    assert_eq!(error["error"]["code"], "forbidden");
    assert_eq!(success(&p, &["request", "show", "task"]), task);
}

#[test]
fn required_arguments_fail_without_interactive_prompt_or_mutation() {
    let dir = tempfile::tempdir().unwrap();
    success(dir.path(), &["workspace", "init", "--name", "本人"]);
    let failure = invoke(dir.path(), &["worker", "create"]);
    assert!(!failure.status.success());
    assert!(failure.stderr.is_empty());
    let result: Value = serde_json::from_slice(&failure.stdout).unwrap();
    assert_eq!(result["error"]["code"], "invalid_request");
    for extra in [vec![], vec!["--verification", "v", "--failed-run", "r"]] {
        let mut args = vec![
            "--request-id",
            "invalid-rework",
            "task",
            "rework",
            "missing-task",
            "--revision",
            "1",
            "--instruction",
            "继续",
        ];
        args.extend(extra);
        let rejected = invoke(dir.path(), &args);
        assert!(!rejected.status.success());
        assert!(rejected.stderr.is_empty());
        let error: Value = serde_json::from_slice(&rejected.stdout).unwrap();
        assert_eq!(error["error"]["code"], "invalid_request");
    }
    let conflict = invoke(
        dir.path(),
        &[
            "--request-id",
            "invalid-recheck",
            "task",
            "verify",
            "missing-task",
            "--revision",
            "1",
            "--artifact",
            "artifact",
            "--instruction",
            "重新检验",
            "--blocker",
            "b",
            "--inconclusive",
            "v",
        ],
    );
    assert!(!conflict.status.success());
    assert!(conflict.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.stdout).unwrap()["error"]["code"],
        "invalid_request"
    );
    let without_id = invoke(dir.path(), &["worker", "create", "--name", "未提交"]);
    assert!(!without_id.status.success());
    assert_eq!(
        success(dir.path(), &["worker", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn unsupported_schema_is_not_reinitialized_and_doctor_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    success(dir.path(), &["workspace", "init", "--name", "本人"]);
    let db = rusqlite::Connection::open(dir.path().join("atelier.sqlite3")).unwrap();
    db.pragma_update(None, "user_version", 99).unwrap();
    let result = invoke(dir.path(), &["doctor"]);
    assert!(!result.status.success());
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        99
    );
    assert!(
        !invoke(dir.path(), &["workspace", "init", "--name", "替换"])
            .status
            .success()
    );
}

#[test]
fn patch_replay_after_later_change_returns_original_result() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    success(p, &["workspace", "init", "--name", "本人"]);
    let worker = success(
        p,
        &[
            "--request-id",
            "create",
            "worker",
            "create",
            "--name",
            "原名称",
        ],
    );
    let id = worker["id"].as_str().unwrap();
    let patch = [
        "--request-id",
        "patch",
        "worker",
        "update",
        id,
        "--revision",
        "1",
        "--description",
        "职责",
    ];
    let first = success(p, &patch);
    success(
        p,
        &[
            "--request-id",
            "second",
            "worker",
            "update",
            id,
            "--revision",
            "2",
            "--name",
            "新名称",
        ],
    );
    assert_eq!(success(p, &patch), first);
    assert_eq!(success(p, &["worker", "show", id])["name"], "新名称");
}

#[test]
fn concurrent_cli_submission_deduplicates_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let init = success(p, &["workspace", "init", "--name", "本人"]);
    let id = init["self"]["id"].as_str().unwrap();
    let team = success(
        p,
        &[
            "--request-id",
            "team",
            "team",
            "create",
            "--name",
            "团队",
            "--members",
            id,
            "--leader",
            id,
        ],
    );
    let team_id = team["id"].as_str().unwrap();
    let args = [
        "--request-id",
        "same",
        "task",
        "create",
        "--team",
        team_id,
        "--goal",
        "并发目标",
    ];
    let mut children = vec![];
    for _ in 0..4 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_atelier"))
                .arg("--workspace")
                .arg(p)
                .arg("--json")
                .args(args)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let values: Vec<Value> = children
        .into_iter()
        .map(|child| {
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            serde_json::from_slice(&output.stdout).unwrap()
        })
        .collect();
    assert!(values.iter().all(|v| v == &values[0]));
    assert_eq!(success(p, &["task", "list"]).as_array().unwrap().len(), 1);
    assert_eq!(
        success(p, &["mailbox", "list"]).as_array().unwrap().len(),
        1
    );
}

#[test]
fn permission_revoke_is_atomic_versioned_and_cannot_be_bypassed_by_cache() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let init = success(p, &["workspace", "init", "--name", "本人"]);
    let human = init["self"]["id"].as_str().unwrap();
    let team = success(
        p,
        &[
            "--request-id",
            "team",
            "team",
            "create",
            "--name",
            "团队",
            "--members",
            human,
            "--leader",
            human,
        ],
    );
    let id = team["id"].as_str().unwrap();
    let create = [
        "--request-id",
        "task",
        "task",
        "create",
        "--team",
        id,
        "--goal",
        "任务",
    ];
    success(p, &create);
    let permission = format!("{human}:task.communicate");
    let overlap = invoke(
        p,
        &[
            "--request-id",
            "overlap",
            "team",
            "permissions",
            "update",
            id,
            "--revision",
            "1",
            "--grant",
            &permission,
            "--revoke",
            &permission,
        ],
    );
    assert!(!overlap.status.success());
    assert_eq!(
        success(p, &["team", "show", id])["authorization_revision"],
        1
    );
    let changed = success(
        p,
        &[
            "--request-id",
            "revoke",
            "team",
            "permissions",
            "update",
            id,
            "--revision",
            "1",
            "--revoke",
            &permission,
        ],
    );
    assert_eq!(changed["team"]["authorization_revision"], 2);
    assert!(!invoke(p, &create).status.success());
    assert!(!invoke(p, &["request", "show", "task"]).status.success());
    success(
        p,
        &[
            "--request-id",
            "restore",
            "team",
            "permissions",
            "update",
            id,
            "--revision",
            "2",
            "--grant",
            &permission,
        ],
    );
    success(p, &create);
    assert_eq!(success(p, &["task", "list"]).as_array().unwrap().len(), 1);
}

struct RuntimeCleanup(std::path::PathBuf);
impl Drop for RuntimeCleanup {
    fn drop(&mut self) {
        let _ = invoke(&self.0, &["runtime", "stop"]);
    }
}

#[test]
fn sample_preparation_is_reproducible_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("first");
    let b = dir.path().join("second");
    let prepare = |path: &Path| {
        success(
            dir.path(),
            &["sample", "prepare", "--destination", path.to_str().unwrap()],
        )
    };
    let first = prepare(&a);
    let second = prepare(&b);
    assert_eq!(first["commit"], second["commit"]);
    assert_eq!(first["taskCreated"], false);
    assert_eq!(
        std::fs::read(a.join("index.html")).unwrap(),
        std::fs::read(b.join("index.html")).unwrap()
    );
    let clean = Command::new("git")
        .arg("-C")
        .arg(&a)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(clean.status.success());
    assert!(clean.stdout.is_empty());
    let before = std::fs::read(a.join("index.html")).unwrap();
    assert!(
        !invoke(
            dir.path(),
            &["sample", "prepare", "--destination", a.to_str().unwrap()]
        )
        .status
        .success()
    );
    assert_eq!(std::fs::read(a.join("index.html")).unwrap(), before);
    assert!(!dir.path().join("atelier.sqlite3").exists());
}

#[test]
fn runtime_survives_start_client_and_blocks_unconfigured_agent_without_spending_run() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    let initialized = success(p, &["workspace", "init", "--name", "本人"]);
    let human = initialized["self"]["id"].as_str().unwrap();
    let worker = success(
        p,
        &[
            "--request-id",
            "worker",
            "worker",
            "create",
            "--name",
            "团队负责人",
        ],
    );
    let leader = worker["id"].as_str().unwrap();
    let members = format!("{human},{leader}");
    let team = success(
        p,
        &[
            "--request-id",
            "team",
            "team",
            "create",
            "--name",
            "团队",
            "--members",
            &members,
            "--leader",
            leader,
        ],
    );
    let team_id = team["id"].as_str().unwrap();
    let task = success(
        p,
        &[
            "--request-id",
            "task",
            "task",
            "create",
            "--team",
            team_id,
            "--goal",
            "排队目标",
        ],
    );
    let _cleanup = RuntimeCleanup(p.into());
    let started = success(p, &["runtime", "start"]);
    assert_eq!(started["state"], "running");
    assert_eq!(started["executionSupported"], true);
    assert_eq!(
        started["executionTransports"],
        serde_json::json!(["api", "agent-cli"])
    );
    let status = success(p, &["runtime", "status"]);
    assert_eq!(status["epoch"], started["epoch"]);
    assert_eq!(status["lockHeld"], true);
    assert_eq!(success(p, &["runtime", "start"])["epoch"], started["epoch"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if success(p, &["mailbox", "list", "--worker", leader])[0]["status"] == "blocked" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "缺配置应明确阻塞");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let stopped = success(p, &["runtime", "stop"]);
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["lockHeld"], false);
    assert_eq!(
        success(p, &["task", "show", task["task"]["id"].as_str().unwrap()])["state"],
        "pending"
    );
    let queue = success(p, &["mailbox", "list", "--worker", leader]);
    assert_eq!(queue.as_array().unwrap().len(), 1);
    assert_eq!(queue[0]["status"], "blocked");
    assert!(queue[0]["reason"].as_str().unwrap().contains("未配置"));
    let task = success(p, &["task", "show", task["task"]["id"].as_str().unwrap()]);
    assert_eq!(task["runs_used"], 0);
    let human_queue = success(p, &["mailbox", "list"]);
    assert_eq!(human_queue.as_array().unwrap().len(), 1);
    assert_eq!(human_queue[0]["status"], "queued");
    assert_eq!(human_queue[0]["message"]["kind"], "decision.request");
    let recovery: Value =
        serde_json::from_str(human_queue[0]["message"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(recovery["kind"], "recovery");
}

#[test]
fn concurrent_start_does_not_leave_a_waiting_second_service() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    success(p, &["workspace", "init", "--name", "本人"]);
    let _cleanup = RuntimeCleanup(p.into());
    let mut children = vec![];
    for _ in 0..4 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_atelier"))
                .arg("--workspace")
                .arg(p)
                .args(["--json", "runtime", "start"])
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    assert_eq!(success(p, &["runtime", "status"])["state"], "running");
    let stopped = success(p, &["runtime", "stop"]);
    assert_eq!(stopped["state"], "stopped");
    std::thread::sleep(std::time::Duration::from_millis(550));
    let status = success(p, &["runtime", "status"]);
    assert_eq!(status["epoch"], stopped["epoch"]);
    assert_eq!(status["state"], "stopped");
}

#[test]
fn killed_owned_service_is_reported_interrupted_and_restart_uses_new_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    success(p, &["workspace", "init", "--name", "本人"]);
    let _cleanup = RuntimeCleanup(p.into());
    let mut child = Command::new(env!("CARGO_BIN_EXE_atelier"))
        .arg("--workspace")
        .arg(p)
        .args(["runtime-serve", "--epoch", "crash-test"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let state = success(p, &["runtime", "status"]);
        if state["state"] == "running" && state["epoch"] == "crash-test" {
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("测试服务未启动");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(success(p, &["runtime", "status"])["state"], "interrupted");
    let started = success(p, &["runtime", "start"]);
    assert_eq!(started["state"], "running");
    assert_ne!(started["epoch"], "crash-test");
    assert_eq!(success(p, &["runtime", "stop"])["state"], "stopped");
}

#[test]
fn fixed_input_import_ignores_worktree_and_replays_after_source_disappears() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let repository = dir.path().join("source with spaces");
    success(&workspace, &["workspace", "init", "--name", "本人"]);
    let sample = success(
        &workspace,
        &[
            "sample",
            "prepare",
            "--destination",
            repository.to_str().unwrap(),
        ],
    );
    let commit = sample["commit"].as_str().unwrap();
    let original = std::fs::read(repository.join("index.html")).unwrap();
    std::fs::write(
        repository.join("index.html"),
        "uncommitted different content",
    )
    .unwrap();
    std::fs::write(repository.join("untracked.txt"), "must not import").unwrap();
    let args = [
        "--request-id",
        "input",
        "input",
        "import",
        "--repository",
        repository.to_str().unwrap(),
        "--commit",
        commit,
    ];
    let saved = success(&workspace, &args);
    let files = saved["files"].as_array().unwrap();
    assert!(!files.iter().any(|file| file["path"] == "untracked.txt"));
    let html = files
        .iter()
        .find(|file| file["path"] == "index.html")
        .unwrap();
    let blob = workspace
        .join("objects/blobs")
        .join(html["sha256"].as_str().unwrap());
    assert_eq!(std::fs::read(blob).unwrap(), original);
    std::fs::rename(&repository, dir.path().join("moved source")).unwrap();
    assert_eq!(success(&workspace, &args), saved);
    assert_eq!(
        success(
            &workspace,
            &["input", "show", saved["id"].as_str().unwrap()]
        ),
        saved
    );
    assert_eq!(
        success(&workspace, &["input", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        !invoke(
            &workspace,
            &[
                "--request-id",
                "bad",
                "input",
                "import",
                "--repository",
                repository.to_str().unwrap(),
                "--commit",
                "HEAD"
            ]
        )
        .status
        .success()
    );
}

#[test]
fn profile_import_rejects_mutable_images_and_unknown_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    success(&workspace, &["workspace", "init", "--name", "本人"]);
    let profile_path = dir.path().join("profile.json");
    let mut profile = serde_json::json!({"name":"独立检查", "check_id":"tic-tac-toe", "image":"trusted:latest", "argv":["node", "/checks/check.mjs", "/candidate"]});
    let args = [
        "--request-id",
        "profile",
        "profile",
        "import",
        "--file",
        profile_path.to_str().unwrap(),
    ];
    std::fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();
    assert!(!invoke(&workspace, &args).status.success());
    profile["image"] = format!("sha256:{}", "b".repeat(64)).into();
    profile["allow_network"] = true.into();
    std::fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();
    assert!(!invoke(&workspace, &args).status.success());
    profile.as_object_mut().unwrap().remove("allow_network");
    std::fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();
    let saved = success(&workspace, &args);
    assert_eq!(success(&workspace, &args), saved);
    assert_eq!(
        success(
            &workspace,
            &["profile", "show", saved["id"].as_str().unwrap()]
        ),
        saved
    );
    assert_eq!(
        success(&workspace, &["profile", "list"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    profile["name"] = "changed".into();
    std::fs::write(&profile_path, serde_json::to_vec(&profile).unwrap()).unwrap();
    assert!(!invoke(&workspace, &args).status.success());
    // Transport support describes installed dispatch, not this profile readiness.
    assert_eq!(
        success(&workspace, &["workspace", "show"])["capabilities"]["executionTransports"],
        serde_json::json!(["api", "agent-cli"])
    );
}

#[cfg(unix)]
#[test]
fn git_symlink_is_rejected_without_publishing_input_or_following_target() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let repository = dir.path().join("source");
    success(&workspace, &["workspace", "init", "--name", "本人"]);
    success(
        &workspace,
        &[
            "sample",
            "prepare",
            "--destination",
            repository.to_str().unwrap(),
        ],
    );
    std::os::unix::fs::symlink("/private/never-read-this", repository.join("link")).unwrap();
    for args in [
        vec!["add", "link"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-m",
            "synthetic symlink",
        ],
    ] {
        assert!(
            Command::new("git")
                .current_dir(&repository)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let output = Command::new("git")
        .current_dir(&repository)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    let commit = String::from_utf8(output.stdout).unwrap();
    let output = invoke(
        &workspace,
        &[
            "--request-id",
            "symlink",
            "input",
            "import",
            "--repository",
            repository.to_str().unwrap(),
            "--commit",
            commit.trim(),
        ],
    );
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "invalid_request");
    assert_eq!(
        success(&workspace, &["input", "list"]),
        serde_json::json!([])
    );
    assert!(
        !invoke(&workspace, &["request", "show", "symlink"])
            .status
            .success()
    );
}

#[test]
fn formal_decision_cli_persists_response_and_separate_implementation() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("workspace");
    let human = success(&p, &["workspace", "init", "--name", "本人"])["self"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let team = success(
        &p,
        &[
            "--request-id",
            "team",
            "team",
            "create",
            "--name",
            "报告团队",
            "--members",
            &human,
            "--leader",
            &human,
        ],
    );
    let task = success(
        &p,
        &[
            "--request-id",
            "task",
            "task",
            "create",
            "--team",
            team["id"].as_str().unwrap(),
            "--goal",
            "编写报告",
        ],
    );
    let task_id = task["task"]["id"].as_str().unwrap();
    let request_args = [
        "--request-id",
        "ask",
        "task",
        "decision",
        "request",
        "--task",
        task_id,
        "--revision",
        "1",
        "--handler",
        &human,
        "--question",
        "报告使用哪一种资料？",
        "--impact",
        "答复不自动改变任务输入",
        "--option",
        "公开资料",
        "--option",
        "继续等待",
    ];
    let request = success(&p, &request_args);
    assert_eq!(success(&p, &request_args), request);
    let id = request["decision"]["id"].as_str().unwrap();
    assert_eq!(
        success(&p, &["task", "decision", "list", "--task", task_id])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let invalid = invoke(
        &p,
        &[
            "--request-id",
            "answer",
            "task",
            "decision",
            "respond",
            id,
            "--revision",
            "1",
            "--answer",
            "不在选项里",
        ],
    );
    assert!(!invalid.status.success());
    let answer_args = [
        "--request-id",
        "answer",
        "task",
        "decision",
        "respond",
        id,
        "--revision",
        "1",
        "--answer",
        "公开资料",
    ];
    let answer = success(&p, &answer_args);
    assert_eq!(answer["decision"]["state"], "responded");
    assert_eq!(success(&p, &answer_args), answer);
    assert_eq!(
        success(&p, &["task", "show", task_id])["contract"]["inputs"],
        ""
    );
    assert!(
        !invoke(
            &p,
            &[
                "--request-id",
                "missing-mode",
                "task",
                "decision",
                "record",
                id,
                "--revision",
                "2"
            ]
        )
        .status
        .success()
    );
    success(
        &p,
        &[
            "--request-id",
            "update",
            "task",
            "update",
            task_id,
            "--revision",
            "1",
            "--inputs",
            "只使用公开资料",
            "--decision",
            id,
        ],
    );
    let pending = success(&p, &["task", "decision", "show", id]);
    assert_eq!(pending["state"], "responded");
    let revision = pending["revision"].to_string();
    let result = success(
        &p,
        &[
            "--request-id",
            "record",
            "task",
            "decision",
            "record",
            id,
            "--revision",
            &revision,
            "--operation-actor",
            &human,
            "--operation-request",
            "update",
        ],
    );
    assert_eq!(result["state"], "resolved");
    assert_eq!(
        success(&p, &["task", "show", task_id])["contract"]["inputs"],
        "只使用公开资料"
    );
    assert_eq!(success(&p, &["task", "show", task_id])["state"], "pending");
}

#[test]
fn connection_cli_preserves_versions_and_can_bind_an_existing_worker() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("workspace");
    success(&p, &["workspace", "init", "--name", "本人"]);
    let worker = success(
        &p,
        &[
            "--request-id",
            "worker",
            "worker",
            "create",
            "--name",
            "先保存身份",
        ],
    );
    let file = dir.path().join("api.json");
    std::fs::write(&file,r#"{"transport":"api","protocol":"openai-chat-completions","model":"model-one","base_url":"https://example.invalid/v1"}"#).unwrap();
    let created = success(
        &p,
        &[
            "--request-id",
            "connection",
            "connection",
            "create",
            "--name",
            "测试连接",
            "--file",
            file.to_str().unwrap(),
        ],
    );
    let id = created["connection"]["id"].as_str().unwrap();
    let version = created["version"]["id"].as_str().unwrap();
    let bound = success(
        &p,
        &[
            "--request-id",
            "bind",
            "worker",
            "update",
            worker["id"].as_str().unwrap(),
            "--revision",
            "1",
            "--connection",
            id,
        ],
    );
    let config = bound["execution_config"].clone();
    assert!(config.is_string());
    let description = success(
        &p,
        &[
            "--request-id",
            "description",
            "worker",
            "update",
            worker["id"].as_str().unwrap(),
            "--revision",
            "2",
            "--description",
            "修改工作说明，不替换模型连接",
        ],
    );
    assert_eq!(description["execution_config"], config);
    std::fs::write(
        &file,
        r#"{"transport":"api","protocol":"openai-chat-completions","model":"model-two"}"#,
    )
    .unwrap();
    let changed = success(
        &p,
        &[
            "--request-id",
            "update",
            "connection",
            "update",
            id,
            "--revision",
            "1",
            "--file",
            file.to_str().unwrap(),
        ],
    );
    assert_ne!(changed["version"]["id"], version);
    assert_eq!(
        success(&p, &["connection", "version", version])["specification"]["model"],
        "model-one"
    );
    assert_eq!(
        success(&p, &["worker", "show", worker["id"].as_str().unwrap()])["execution_config"],
        config
    );
    assert_eq!(
        success(&p, &["connection", "show", id])["readiness"],
        "unchecked"
    );
    assert_eq!(success(&p, &["task", "list"]), serde_json::json!([]));
    assert!(
        !invoke(
            &p,
            &[
                "--request-id",
                "bad-clear",
                "worker",
                "update",
                worker["id"].as_str().unwrap(),
                "--revision",
                "3",
                "--connection",
                id,
                "--clear-connection"
            ]
        )
        .status
        .success()
    );
}

#[test]
fn task_execute_cli_persists_a_blocked_assignment_without_starting_a_run() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("workspace");
    let human = success(&p, &["workspace", "init", "--name", "本人"])["self"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let executor = success(
        &p,
        &[
            "--request-id",
            "e",
            "worker",
            "create",
            "--name",
            "执行成员",
        ],
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let verifier = success(
        &p,
        &[
            "--request-id",
            "v",
            "worker",
            "create",
            "--name",
            "检验成员",
        ],
    )["id"]
        .as_str()
        .unwrap()
        .to_string();
    let members = format!("{human},{executor},{verifier}");
    let team = success(
        &p,
        &[
            "--request-id",
            "team",
            "team",
            "create",
            "--name",
            "团队",
            "--members",
            &members,
            "--leader",
            &human,
            "--executor",
            &executor,
            "--verifier",
            &verifier,
        ],
    );
    let task = success(
        &p,
        &[
            "--request-id",
            "task",
            "task",
            "create",
            "--team",
            team["id"].as_str().unwrap(),
            "--goal",
            "完成公开合成任务",
        ],
    );
    let id = task["task"]["id"].as_str().unwrap();
    success(
        &p,
        &[
            "--request-id",
            "update",
            "task",
            "update",
            id,
            "--revision",
            "1",
            "--inputs",
            "输入齐备",
            "--delivery",
            "任务结果",
            "--verification",
            "独立核对",
        ],
    );
    success(
        &p,
        &[
            "--request-id",
            "intake",
            "task",
            "intake",
            id,
            "--revision",
            "2",
            "--decision",
            "accept",
            "--reason",
            "契约齐备",
        ],
    );
    let args = [
        "--request-id",
        "arrange",
        "task",
        "execute",
        id,
        "--revision",
        "3",
        "--instruction",
        "按契约执行",
    ];
    let arranged = success(&p, &args);
    assert_eq!(arranged["delivery"]["status"], "blocked");
    assert_eq!(success(&p, &args), arranged);
    let mailbox = success(&p, &["mailbox", "list", "--worker", &executor]);
    assert_eq!(mailbox.as_array().unwrap().len(), 1);
    assert_eq!(mailbox[0]["message"]["kind"], "assignment.execute");
    assert_eq!(mailbox[0]["status"], "blocked");
    assert_eq!(success(&p, &["task", "show", id])["runs_used"], 0);
    assert!(
        !invoke(
            &p,
            &[
                "--request-id",
                "duplicate",
                "task",
                "execute",
                id,
                "--revision",
                "3",
                "--instruction",
                "新请求不能重复安排"
            ]
        )
        .status
        .success()
    );
}

#[test]
fn connection_test_cli_saves_diagnostics_and_does_not_create_business_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace");
    success(&path, &["workspace", "init", "--name", "本人"]);
    let config = dir.path().join("api.json");
    std::fs::write(&config,r#"{"transport":"api","protocol":"openai-chat-completions","model":"fixture","base_url":"https://example.invalid"}"#).unwrap();
    let created = success(
        &path,
        &[
            "--request-id",
            "connection",
            "connection",
            "create",
            "--name",
            "API",
            "--file",
            config.to_str().unwrap(),
        ],
    );
    let id = created["connection"]["id"].as_str().unwrap();
    assert!(
        !invoke(&path, &["connection", "test", id, "--revision", "1"])
            .status
            .success()
    );
    let tested = success(
        &path,
        &[
            "--request-id",
            "diagnostic",
            "connection",
            "test",
            id,
            "--revision",
            "1",
        ],
    );
    assert_eq!(tested["state"], "failed");
    assert_eq!(tested["code"], "credential_missing");
    assert_eq!(
        success(
            &path,
            &[
                "--request-id",
                "diagnostic",
                "connection",
                "test",
                id,
                "--revision",
                "1"
            ]
        ),
        tested
    );
    assert_eq!(success(&path, &["request", "show", "diagnostic"]), tested);
    assert_eq!(
        success(&path, &["connection", "show", id])["readiness"],
        "unavailable"
    );
    assert!(
        !invoke(
            &path,
            &[
                "--request-id",
                "wrong-revision",
                "connection",
                "test",
                id,
                "--revision",
                "2"
            ]
        )
        .status
        .success()
    );
    assert!(
        !invoke(
            &path,
            &[
                "--request-id",
                "api-worker",
                "connection",
                "test",
                id,
                "--revision",
                "1",
                "--worker",
                "unused"
            ]
        )
        .status
        .success()
    );
    std::fs::write(
        &config,
        r#"{"transport":"agent-cli","runtime":"pi","model":null,"image":null}"#,
    )
    .unwrap();
    let cli = success(
        &path,
        &[
            "--request-id",
            "cli",
            "connection",
            "create",
            "--name",
            "CLI",
            "--file",
            config.to_str().unwrap(),
        ],
    );
    let cli_id = cli["connection"]["id"].as_str().unwrap();
    assert!(
        !invoke(
            &path,
            &[
                "--request-id",
                "no-worker",
                "connection",
                "test",
                cli_id,
                "--revision",
                "1"
            ]
        )
        .status
        .success()
    );
    let worker = success(
        &path,
        &[
            "--request-id",
            "worker",
            "worker",
            "create",
            "--name",
            "专用环境成员",
            "--connection",
            cli_id,
        ],
    );
    let unsupported = success(
        &path,
        &[
            "--request-id",
            "cli-diagnostic",
            "connection",
            "test",
            cli_id,
            "--revision",
            "1",
            "--worker",
            worker["id"].as_str().unwrap(),
        ],
    );
    assert_eq!(unsupported["state"], "unsupported");
    assert_eq!(unsupported["code"], "agent_cli_unavailable");
    for object in ["task", "team"] {
        assert!(
            success(&path, &[object, "list"])
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    let db = rusqlite::Connection::open(path.join("atelier.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM runs", [], |r| r.get::<_, u64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM connection_tests", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        2
    );
}

#[test]
fn skill_bootstrap_install_is_idempotent_and_preserves_user_content() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("工作区 尚未初始化");
    let target = dir.path().join("atelier");
    let installed = success(
        &workspace,
        &[
            "skill",
            "install",
            "--destination",
            target.to_str().unwrap(),
        ],
    );
    assert_eq!(installed["unchanged"], false);
    assert!(!workspace.exists(), "安装不自动创建业务工作区");
    assert_eq!(
        success(
            &workspace,
            &[
                "skill",
                "install",
                "--destination",
                target.to_str().unwrap()
            ]
        )["unchanged"],
        true
    );
    let bytes = std::fs::read(target.join("SKILL.md")).unwrap();
    assert_eq!(installed["digest"], atelier::content::digest(&bytes));
    assert_eq!(
        std::fs::read_dir(&target).unwrap().count(),
        1,
        "宿主只安装最小入口"
    );
    std::fs::write(target.join("SKILL.md"), "user modified skill").unwrap();
    assert!(
        !invoke(
            &workspace,
            &[
                "skill",
                "install",
                "--destination",
                target.to_str().unwrap()
            ]
        )
        .status
        .success()
    );
    assert_eq!(
        std::fs::read_to_string(target.join("SKILL.md")).unwrap(),
        "user modified skill"
    );
    #[cfg(unix)]
    {
        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        assert!(
            !invoke(
                &workspace,
                &[
                    "skill",
                    "install",
                    "--destination",
                    linked.to_str().unwrap()
                ]
            )
            .status
            .success()
        );
    }
}

#[test]
fn skill_describe_distinguishes_initialization_corruption_and_protocol_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    let initial = success(&workspace, &["skill", "describe", "--protocol", "2"]);
    assert_eq!(initial["state"], "workspace_missing");
    assert_eq!(initial["operations"][0]["id"], "workspace.init");
    assert!(!workspace.exists());
    assert!(
        !invoke(&workspace, &["skill", "describe", "--protocol", "1"])
            .status
            .success()
    );
    assert!(
        !invoke(
            &workspace,
            &[
                "skill",
                "describe",
                "--protocol",
                "2",
                "--team",
                "unknown",
                "--task",
                "unknown"
            ]
        )
        .status
        .success()
    );
    let initialized = success(&workspace, &["workspace", "init", "--name", "本人"]);
    let ready = success(&workspace, &["skill", "describe", "--protocol", "2"]);
    assert_eq!(ready["state"], "workspace");
    assert_eq!(ready["identity"]["workerId"], initialized["self"]["id"]);
    assert_eq!(ready["permissions"], serde_json::json!([]));
    assert!(
        !ready["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == "workspace.init")
    );
    std::fs::write(workspace.join("atelier.sqlite3"), "corrupt fixture").unwrap();
    let broken = invoke(&workspace, &["skill", "describe", "--protocol", "2"]);
    assert!(!broken.status.success());
    let error: Value = serde_json::from_slice(&broken.stdout).unwrap();
    assert_eq!(error["ok"], false);
    assert!(
        !broken
            .stdout
            .windows(b"workspace_missing".len())
            .any(|b| b == b"workspace_missing")
    );
}
