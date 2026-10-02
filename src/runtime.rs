//! Single-machine service lifecycle and mechanical mailbox dispatch.
use crate::{Error, Result, database::Database, store::Store};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn lock_file(path: &Path) -> Result<File> {
    let file_path = path.join("runtime.lock");
    match std::fs::symlink_metadata(&file_path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err(Error::Invalid("服务锁必须为普通文件".into()));
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(file_path)?)
}

fn acquire(path: &Path) -> Result<File> {
    let lock = lock_file(path)?;
    // Startup parent disposes of a losing child if another service wins. A
    // blocking lock also avoids mistaking a brief status probe for a service.
    lock.lock_exclusive()?;
    Ok(lock)
}

pub fn status(path: &Path) -> Result<Value> {
    let store = Store::open(path)?;
    let lock = lock_file(path)?;
    let held = match FileExt::try_lock_shared(&lock) {
        Ok(()) => false,
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => true,
        Err(error) => return Err(error.into()),
    };
    // When the exclusive owner has exited, hold the shared lock before reading
    // its final transaction. Reading first can combine stale 'running' with a
    // released lock and falsely report interruption after a successful stop.
    let mut snapshot = store.runtime_snapshot()?;
    if held && snapshot["state"] == "not_started" {
        snapshot["state"] = json!("starting");
    } else if held && (snapshot["stopRequested"] == true || snapshot["state"] == "stopped") {
        snapshot["state"] = json!("stopping");
    } else if !held && snapshot["state"] == "running" {
        snapshot["state"] = json!("interrupted");
    }
    snapshot["lockHeld"] = json!(held);
    Ok(snapshot)
}

pub fn start(path: &Path) -> Result<Value> {
    let existing = status(path)?;
    if existing["lockHeld"] == true {
        return Ok(existing);
    }
    let observed_epoch = existing["epoch"].clone();
    let path = std::fs::canonicalize(path)?;
    let epoch = uuid::Uuid::new_v4().to_string();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--workspace")
        .arg(&path)
        .arg("runtime-serve")
        .arg("--epoch")
        .arg(&epoch)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let snapshot = status(&path)?;
        if snapshot["epoch"] == epoch
            && snapshot["state"] == "running"
            && snapshot["lockHeld"] == true
        {
            return Ok(snapshot);
        }
        if snapshot["epoch"] != epoch
            && snapshot["epoch"] != observed_epoch
            && snapshot["state"] == "running"
            && snapshot["lockHeld"] == true
        {
            child.kill()?;
            child.wait()?;
            return Ok(snapshot);
        }
        if child.try_wait()?.is_some() {
            if snapshot["lockHeld"] == true {
                return Ok(snapshot);
            }
            return Err(Error::Unavailable(
                "服务启动失败，请检查工作区与数据库".into(),
            ));
        }
        if Instant::now() >= deadline {
            // Only kill the child owned by this start request, never a recorded PID.
            child.kill()?;
            child.wait()?;
            return Err(Error::Unavailable("服务未在启动期限内就绪".into()));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub fn stop(path: &Path) -> Result<Value> {
    let snapshot = status(path)?;
    if snapshot["lockHeld"] != true {
        return Ok(snapshot);
    }
    let epoch = snapshot["epoch"]
        .as_str()
        .ok_or_else(|| Error::Conflict("服务正在启动，请稍后重试".into()))?
        .to_string();
    Store::open(path)?.runtime_request_stop(&epoch)?;
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        let snapshot = status(path)?;
        if snapshot["epoch"] != epoch {
            return Err(Error::Conflict("服务 epoch 已变化，未停止新服务".into()));
        }
        if snapshot["lockHeld"] != true {
            return Ok(snapshot);
        }
        if Instant::now() >= deadline {
            return Ok(snapshot);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

pub async fn serve(path: &Path, epoch: String) -> Result<()> {
    let _lock = acquire(path)?;
    let database = Database::open(path.to_path_buf(), 32).await?;
    let client = database.client();
    let register = epoch.clone();
    client
        .call(move |store| store.runtime_register(&register, std::process::id()))
        .await?;
    let result = async {
        client.runtime_recover_checks(epoch.clone()).await?;
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            crate::cli_resources::recover(&client, &epoch).await?;
            crate::api_driver::recover(&client, &epoch).await?;
            let current = epoch.clone();
            if client
                .call(move |store| store.runtime_should_stop(&current))
                .await?
            {
                if client
                    .call(|store| crate::runs::active_count(&store.connection))
                    .await?
                    == 0
                {
                    break;
                }
                continue;
            }
            let current = epoch.clone();
            if let Some(work) = client.call(move |store| store.next_work(&current)).await? {
                crate::api_driver::dispatch(
                    client.clone(),
                    epoch.clone(),
                    work,
                    path.to_path_buf(),
                )
                .await?;
            }
        }
        client
            .call(move |store| store.runtime_stopped(&epoch))
            .await
    }
    .await;
    drop(client);
    let drained = database.close().await;
    result.and(drained)
}

/// Offline resource inspection owns the same lock as the service. It never
/// dispatches queued work or accepts caller-supplied stop observations.
pub async fn reconcile(path: &Path) -> Result<Value> {
    Store::open(path)?;
    let lock = lock_file(path)?;
    FileExt::try_lock_exclusive(&lock).map_err(|error| {
        if error.kind() == std::io::ErrorKind::WouldBlock {
            Error::Conflict("运行服务仍持有工作区；先 runtime stop，待服务退出后再核对".into())
        } else {
            error.into()
        }
    })?;
    let database = Database::open(path.to_path_buf(), 32).await?;
    let client = database.client();
    let epoch = uuid::Uuid::new_v4().to_string();
    let result = async {
        let owner = epoch.clone();
        client
            .call(move |store| {
                store.runtime_register(&owner, std::process::id())?;
                store.runtime_request_stop(&owner)
            })
            .await?;
        let checks = client.runtime_recover_checks(epoch.clone()).await?;
        let cli_resources = crate::cli_resources::recover(&client, &epoch).await?;
        crate::api_driver::recover(&client, &epoch).await?;
        client
            .call(move |store| {
                crate::runs::service(&store.connection, &epoch, false)?;
                let active = crate::runs::active_count(&store.connection)?;
                let state = if active == 0 {
                    "stopped"
                } else {
                    "blocked_unknown"
                };
                store.connection.execute(
                    "UPDATE runtime SET state=?2,stop_requested=1 WHERE singleton=1 AND epoch=?1",
                    rusqlite::params![epoch, state],
                )?;
                let mut result = store.runtime_snapshot()?;
                let mut statement = store
                    .connection
                    .prepare("SELECT id FROM runs WHERE state!='stopped' ORDER BY rowid")?;
                let pending = statement
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                result["unresolvedRunIds"] = json!(pending);
                result["reconciledChecks"] = json!(checks);
                result["reconciledCliResources"] = json!(cli_resources);
                result["next"] = json!(if active == 0 {
                    "资源已核对；查询投递及恢复待办，按实际决定继续；未领取新工作"
                } else {
                    "仍有资源无法确认停止；查询 run show，不重试未知工作"
                });
                Ok(result)
            })
            .await
    }
    .await;
    drop(client);
    database.close().await?;
    result
}
