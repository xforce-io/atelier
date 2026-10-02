//! Owned Docker checks, always outside the DB thread. Candidate programs never
//! execute on the host and cannot choose Docker options, mounts or commands.
use crate::{
    Error, Result, content,
    database::DatabaseClient,
    verification::{CheckPlan, Observation, SavedObservation},
};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    sync::watch,
};
const LIMIT: usize = 5 * 1024 * 1024;
struct Output {
    code: Option<i32>,
    out: Vec<u8>,
    err: Vec<u8>,
    out_truncated: bool,
    err_truncated: bool,
    interrupted: bool,
}
async fn collect<R: AsyncRead + Unpin>(mut stream: R) -> std::io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut truncated = false;
    loop {
        let size = stream.read(&mut chunk).await?;
        if size == 0 {
            break;
        }
        let take = size.min(LIMIT.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&chunk[..take]);
        truncated |= take < size;
    }
    Ok((bytes, truncated))
}
async fn docker(args: &[String], seconds: u64, mut stop: watch::Receiver<bool>) -> Result<Output> {
    let mut command = Command::new("docker");
    command.env_clear();
    for key in [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
    ] {
        if let Some(v) = std::env::var_os(key) {
            command.env(key, v);
        }
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let out = tokio::spawn(collect(child.stdout.take().unwrap()));
    let err = tokio::spawn(collect(child.stderr.take().unwrap()));
    let mut interrupted = false;
    let status = tokio::select! {
        status=child.wait()=>status?,
        _=tokio::time::sleep(Duration::from_secs(seconds))=>{interrupted=true;child.start_kill()?;child.wait().await?},
        _=async {loop {if *stop.borrow() {break;}
            if stop.changed().await.is_err() {std::future::pending::<()>().await;}}}=>{interrupted=true;child.start_kill()?;child.wait().await?},
    };
    let (out, out_truncated) = out
        .await
        .map_err(|_| Error::Unavailable("检查输出读取任务失败".into()))??;
    let (err, err_truncated) = err
        .await
        .map_err(|_| Error::Unavailable("检查错误输出读取任务失败".into()))??;
    Ok(Output {
        code: status.code(),
        out,
        err,
        out_truncated,
        err_truncated,
        interrupted,
    })
}
async fn control(args: &[&str]) -> Result<Output> {
    let (_tx, rx) = watch::channel(false);
    docker(
        &args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        15,
        rx,
    )
    .await
}
fn safe_directory(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(Error::Invalid("检查准备目录无效".into()));
    }
    Ok(())
}
fn materialize(plan: &CheckPlan) -> Result<PathBuf> {
    let parent = plan.workspace.join("checks");
    match std::fs::create_dir(&parent) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    };
    safe_directory(&parent)?;
    let root = parent.join(&plan.check.id);
    std::fs::create_dir(&root)?;
    if let Err(error) = materialize_into(plan, &root) {
        let _ = std::fs::remove_dir_all(&root);
        return Err(error);
    }
    Ok(root)
}
fn materialize_into(plan: &CheckPlan, root: &Path) -> Result<()> {
    let a = &plan.input;
    if a.files.len() > 10000
        || a.total_bytes > content::INPUT_LIMIT
        || crate::candidate::manifest(&a.files)? != a.total_bytes
        || content::digest(&serde_json::to_vec(&a.files)?) != plan.check.content_digest
    {
        return Err(Error::Conflict("固定检查输入清单损坏".into()));
    }
    let mut total = 0u64;
    for f in &a.files {
        let bytes = crate::candidate::read_blob(&plan.workspace, f)?;
        total = total
            .checked_add(f.size)
            .ok_or_else(|| Error::Invalid("产出大小超限".into()))?;
        if total > content::INPUT_LIMIT {
            return Err(Error::Invalid("产出大小超限".into()));
        }
        let path = root.join(&f.path);
        std::fs::create_dir_all(path.parent().unwrap())?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(if f.executable { 0o555 } else { 0o444 }),
            )?;
        }
    }
    if total != a.total_bytes {
        return Err(Error::Conflict("固定检查输入大小不匹配".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
            for entry in std::fs::read_dir(&dir)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    dirs.push(entry.path());
                }
            }
        }
    }
    Ok(())
}
async fn inspect(plan: &CheckPlan) -> Option<Value> {
    let output = control(&["inspect", &plan.check.container_name])
        .await
        .ok()?;
    if output.code != Some(0) || output.out_truncated {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.out).ok()?;
    let value = value.as_array()?.first()?.clone();
    (value["Config"]["Labels"]["atelier.check"] == plan.check.id
        && value["Config"]["Labels"]["atelier.run"] == plan.check.run_id)
        .then_some(value)
}
fn isolation(value: &Value) -> Option<Value> {
    let host = &value["HostConfig"];
    let contains = |key: &str, expected: &str| {
        host[key]
            .as_array()
            .is_some_and(|items| items.iter().any(|v| v == expected))
    };
    let mounts = value["Mounts"].as_array()?;
    let tmpfs = host["Tmpfs"].as_object()?;
    let tmp_options = tmpfs.get("/tmp")?.as_str()?.split(',').collect::<Vec<_>>();
    if value["Config"]["User"] != "65534:65534"
        || host["NetworkMode"] != "none"
        || host["ReadonlyRootfs"] != true
        || host["Privileged"] != false
        || host["Memory"] != 536_870_912u64
        || host["MemorySwap"] != 536_870_912u64
        || host["NanoCpus"] != 1_000_000_000u64
        || host["CpusetCpus"] != "0"
        || host["PidsLimit"] != 64
        || !contains("CapDrop", "ALL")
        || !contains("SecurityOpt", "no-new-privileges")
        || mounts.len() != 1
        || mounts[0]["Type"] != "bind"
        || mounts[0]["Destination"] != "/candidate"
        || mounts[0]["RW"] != false
        || tmpfs.len() != 1
        || !["rw", "nosuid", "nodev", "size=134217728", "mode=1777"]
            .iter()
            .all(|v| tmp_options.contains(v))
    {
        return None;
    }
    Some(serde_json::json!({
        "user":value["Config"]["User"],"network":host["NetworkMode"],
        "readonlyRootfs":host["ReadonlyRootfs"],"privileged":host["Privileged"],
        "memoryBytes":host["Memory"],"memorySwapBytes":host["MemorySwap"],
        "nanoCpus":host["NanoCpus"],"cpusetCpus":host["CpusetCpus"],"pidsLimit":host["PidsLimit"],
        "capDrop":host["CapDrop"],"securityOpt":host["SecurityOpt"],
        "candidateReadonly":true,"tmpfs":host["Tmpfs"]
    }))
}
async fn absent(plan: &CheckPlan) -> bool {
    let filter = format!("name=^/{}$", plan.check.container_name);
    match control(&["ps", "-a", "--filter", &filter, "--format", "{{.Names}}"]).await {
        Ok(o) => o.code == Some(0) && !o.out_truncated && o.out.iter().all(u8::is_ascii_whitespace),
        Err(_) => false,
    }
}
async fn cleanup(plan: &CheckPlan) -> bool {
    if absent(plan).await {
        return true;
    }
    if inspect(plan).await.is_none() {
        return false;
    }
    let _ = control(&["rm", "-f", &plan.check.container_name]).await;
    absent(plan).await
}
pub(crate) async fn recover(plan: CheckPlan) -> Result<SavedObservation> {
    let stopped = cleanup(&plan).await;
    let observation = Observation {
        isolation: None,
        exit_code: None,
        resources_stopped: stopped,
        stdout: Vec::new(),
        stderr: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        diagnostic: Some(
            if stopped {
                "旧服务中断；检查资源已核对停止，未保存的检查结果不能推定通过"
            } else {
                "旧服务中断；检查资源归属或停止状态无法核对"
            }
            .into(),
        ),
    };
    tokio::task::spawn_blocking(move || plan.save_observation(observation))
        .await
        .map_err(|_| Error::Unavailable("恢复检查证据固定线程失败".into()))?
}
async fn running_allowed(db: &DatabaseClient, plan: &CheckPlan) -> bool {
    let id = plan.check.run_id.clone();
    let epoch = plan.epoch.clone();
    db.call(move |store| {
        crate::runs::service(&store.connection, &epoch, true)?;
        let run = store.run(&id)?;
        crate::runs::check_run_authority(&store.connection, &run)?;
        Ok(run.state == "running")
    })
    .await
    .unwrap_or(false)
}
pub(crate) async fn execute(plan: CheckPlan, db: DatabaseClient) -> Result<SavedObservation> {
    let workspace = plan.workspace.clone();
    let check_id = plan.check.id.clone();
    let input = plan.input.clone();
    let profile = plan.profile.clone();
    let check = plan.check.clone();
    let epoch = plan.epoch.clone();
    let preparation = CheckPlan {
        workspace,
        check,
        input,
        profile,
        epoch,
    };
    let candidate = tokio::task::spawn_blocking(move || materialize(&preparation))
        .await
        .map_err(|_| Error::Unavailable("检查文件准备线程失败".into()))?;
    let mut observation = Observation {
        isolation: None,
        exit_code: None,
        resources_stopped: true,
        stdout: Vec::new(),
        stderr: Vec::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        diagnostic: None,
    };
    let candidate = match candidate {
        Ok(path) => path,
        Err(_) => {
            observation.diagnostic = Some("固定检查输入无法准备到只读检查目录".into());
            return tokio::task::spawn_blocking(move || plan.save_observation(observation))
                .await
                .map_err(|_| Error::Unavailable("检查证据固定失败".into()))?;
        }
    };
    let mount = format!(
        "type=bind,source={},target=/candidate,readonly",
        candidate.display()
    );
    if mount.contains('\n') || candidate.to_string_lossy().contains(',') {
        observation.diagnostic = Some("检查挂载路径不支持".into());
    } else if !running_allowed(&db, &plan).await {
        observation.diagnostic = Some("检查启动前 Run 已停止或失权".into());
    } else {
        let mut args: Vec<String> = [
            "create",
            "--pull=never",
            "--network",
            "none",
            "--read-only",
            "--user",
            "65534:65534",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--cpus",
            "1",
            "--cpuset-cpus",
            "0",
            "--memory",
            "512m",
            "--memory-swap",
            "512m",
            "--pids-limit",
            "64",
            "--tmpfs",
            "/tmp:rw,nosuid,nodev,size=134217728,mode=1777",
            "--env",
            "XDG_CACHE_HOME=/tmp/cache",
            "--env",
            "HOME=/tmp/home",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        args.extend([
            "--name".into(),
            plan.check.container_name.clone(),
            "--label".into(),
            format!("atelier.check={check_id}"),
            "--label".into(),
            format!("atelier.run={}", plan.check.run_id),
            "--mount".into(),
            mount,
            "--entrypoint".into(),
            plan.profile.specification.argv[0].clone(),
            plan.profile.specification.image.clone(),
        ]);
        args.extend(plan.profile.specification.argv.iter().skip(1).cloned());
        let (_sender, stop) = watch::channel(false);
        let created = docker(&args, 30, stop).await;
        if let Some(state) = inspect(&plan).await {
            observation.isolation = isolation(&state);
        }
        if matches!(&created,Ok(o) if o.code==Some(0)&&!o.interrupted)
            && observation.isolation.is_some()
            && running_allowed(&db, &plan).await
        {
            let (cancel, rx) = watch::channel(false);
            let db_watch = db.clone();
            let id = plan.check.run_id.clone();
            let owner = plan.epoch.clone();
            let watcher = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    let id = id.clone();
                    let owner = owner.clone();
                    let allowed = db_watch
                        .call(move |store| {
                            crate::runs::service(&store.connection, &owner, true)?;
                            let r = store.run(&id)?;
                            crate::runs::check_run_authority(&store.connection, &r)?;
                            Ok(r.state == "running")
                        })
                        .await
                        .unwrap_or(false);
                    if !allowed {
                        let _ = cancel.send(true);
                        break;
                    }
                }
            });
            let output = docker(
                &[
                    "start".into(),
                    "--attach".into(),
                    plan.check.container_name.clone(),
                ],
                120,
                rx,
            )
            .await;
            watcher.abort();
            let _ = watcher.await;
            match output {
                Ok(o) => {
                    observation.stdout = o.out;
                    observation.stderr = o.err;
                    observation.stdout_truncated = o.out_truncated;
                    observation.stderr_truncated = o.err_truncated;
                    if o.interrupted {
                        observation.diagnostic = Some("检查超时或被请求停止".into());
                    }
                }
                Err(_) => observation.diagnostic = Some("检查进程通信失败".into()),
            }
            if let Some(state) = inspect(&plan).await {
                if state["State"]["Running"] == false && state["State"]["OOMKilled"] == false {
                    observation.exit_code = state["State"]["ExitCode"].as_i64();
                } else {
                    observation.diagnostic = Some("检查进程未正常退出或超过内存限制".into());
                }
            } else {
                observation.diagnostic = Some("检查退出状态无法核对".into());
            }
        } else {
            observation.diagnostic =
                Some("检查容器创建失败、归属或隔离配置不符，或启动条件已变化".into());
            if let Ok(o) = created {
                observation.stderr = o.err;
                observation.stderr_truncated = o.err_truncated;
            }
        }
        observation.resources_stopped = cleanup(&plan).await;
    }
    if observation.resources_stopped {
        let owned = candidate;
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(owned)).await;
    }
    tokio::task::spawn_blocking(move || plan.save_observation(observation))
        .await
        .map_err(|_| Error::Unavailable("检查证据固定线程失败".into()))?
}
