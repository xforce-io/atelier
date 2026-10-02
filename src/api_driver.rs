//! One owned API adapter process per accepted Run. Business transitions remain
//! member tool operations; an adapter terminal is only an execution observation.
use crate::{
    Error, Result,
    channel::{Capabilities, ChannelConfiguration},
    connection::ConnectionSpec,
    database::DatabaseClient,
    dispatch::Work,
    execution_context::ExecutionContext,
    model::Run,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

struct Prepared {
    context: ExecutionContext,
    entry: PathBuf,
    connection: Value,
    credential_generation: u64,
}

async fn prepare(
    client: &DatabaseClient,
    epoch: &str,
    work: &Work,
    path: &Path,
) -> Result<Prepared> {
    let configuration = work
        .configuration
        .clone()
        .ok_or_else(|| Error::Unavailable("数字员工未配置执行连接".into()))?;
    let id = configuration.clone();
    let (version, reference) = client
        .call(move |store| {
            let configuration = store.execution_configuration(&id)?;
            let version = store.connection_version(&configuration.connection_version)?;
            let reference = store.credential_reference(&version.id)?;
            Ok((version, reference))
        })
        .await?;
    let ConnectionSpec::Api {
        protocol,
        model,
        base_url,
    } = version.specification
    else {
        return Err(Error::Unavailable("agent CLI 隔离执行尚未接入".into()));
    };
    let reference =
        reference.ok_or_else(|| Error::Unavailable("冻结连接版本缺少本地 API 凭据".into()))?;
    let credential_generation = reference.generation;
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("adapters/milkie/dist/src/main.js");
    let owner = epoch.to_string();
    let task = work.task.id.clone();
    let worker = work.worker_id.clone();
    let purpose = work.purpose.clone();
    let context = client
        .call(move |store| store.runtime_context(&owner, &task, &worker, &configuration, &purpose))
        .await?;
    let directories = context.clone();
    let workspace = path.to_path_buf();
    let entry_check = entry.clone();
    let secret = tokio::task::spawn_blocking(move || {
        if !entry_check.is_file() {
            return Err(Error::Unavailable(
                "API 接入尚未构建，请先构建固定 milkie 接入".into(),
            ));
        }
        directories.prepare_directories(&workspace)?;
        crate::credential::load_secret(&reference)
    })
    .await
    .map_err(|_| Error::Unavailable("API 执行准备线程退出".into()))??;
    let mut connection = json!({"protocol":protocol,"model":model,"apiKey":secret});
    if let Some(base_url) = base_url {
        connection["baseUrl"] = json!(base_url);
    }
    Ok(Prepared {
        context,
        entry,
        connection,
        credential_generation,
    })
}

/// Returns only after all accepted member calls have returned. The outer service
/// must not cancel this future with a timeout while a core mutation is in flight.
pub(crate) async fn dispatch(
    client: DatabaseClient,
    epoch: String,
    work: Work,
    path: PathBuf,
) -> Result<()> {
    if work.message_revision != work.task.revision
        || work.task.state == "closed"
        || work.task.cancellation_requested
    {
        return client
            .call(move |store| {
                store.block_work(&epoch, &work, "投递的任务依据已过期或任务正在取消")
            })
            .await;
    }
    let preparation = match prepare(&client, &epoch, &work, &path).await {
        Ok(prepared) => prepared,
        Err(Error::Database(error)) => return Err(Error::Database(error)),
        Err(error) => {
            return client
                .call(move |store| store.block_work(&epoch, &work, &error.to_string()))
                .await;
        }
    };
    let owner = epoch.clone();
    let delivery = work.delivery_id.clone();
    let configuration = work.configuration.clone().expect("checked configuration");
    let run = match client
        .call(move |store| store.runtime_claim(&owner, &delivery, &configuration))
        .await
    {
        Ok(run) => run,
        Err(Error::Database(error)) => return Err(Error::Database(error)),
        Err(error) => {
            return client
                .call(move |store| store.block_work(&epoch, &work, &error.to_string()))
                .await;
        }
    };
    let result = execute(&client, &epoch, &run, &work, &path, preparation).await;
    // execute owns cleanup, including errors after spawn. Failure here is an
    // unreconciled resource/storage failure, so never release the Run blindly.
    if result.is_err() {
        let owner = epoch.clone();
        let id = run.id.clone();
        client
            .call(move |store| {
                if store.run(&id)?.state != "stopped" {
                    store.runtime_run_unknown(&owner, &id, "API 运行资源或提交效果尚未核对")?;
                }
                Ok(())
            })
            .await?;
    }
    Ok(())
}

async fn finish(client: &DatabaseClient, epoch: &str, run: &Run, reason: &str) -> Result<()> {
    // A check may have survived an error in its own observation/persistence.
    // Artifact publication and observed-stop both refuse such active resources.
    if matches!(run.purpose.as_str(), "execute" | "rework") {
        client
            .runtime_finish_execution(epoch.into(), run.id.clone(), reason.into())
            .await?;
    } else {
        let owner = epoch.to_string();
        let id = run.id.clone();
        let reason = reason.to_string();
        client
            .call(move |store| store.runtime_run_observed_stopped(&owner, &id, &reason))
            .await?;
    }
    Ok(())
}

async fn execute(
    client: &DatabaseClient,
    epoch: &str,
    run: &Run,
    work: &Work,
    path: &Path,
    preparation: Prepared,
) -> Result<()> {
    let preparation_result = if matches!(run.purpose.as_str(), "execute" | "rework") {
        client
            .runtime_prepare_candidate(epoch.into(), run.id.clone())
            .await
    } else {
        Ok(())
    };
    if preparation_result.is_err() {
        let owner = epoch.to_string();
        let id = run.id.clone();
        client
            .call(move |store| {
                store.runtime_run_observed_stopped(&owner, &id, "候选准备失败；未启动执行资源")
            })
            .await?;
        return Ok(());
    }
    let owner = epoch.to_string();
    let id = run.id.clone();
    let context = preparation.context.clone();
    let launch = client
        .call(move |store| {
            store.runtime_begin_api_launch(&owner, &id, &context, preparation.credential_generation)
        })
        .await;
    if launch.is_err() {
        let owner = epoch.to_string();
        let id = run.id.clone();
        client
            .call(move |store| {
                store.runtime_run_observed_stopped(
                    &owner,
                    &id,
                    "启动前配置或授权已变化；没有子进程",
                )
            })
            .await?;
        return Ok(());
    }
    let mut command = tokio::process::Command::new("node");
    command
        .arg(&preparation.entry)
        .arg("--atelier-run")
        .arg(&run.id)
        .current_dir(path)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            finish(client, epoch, run, "API 接入未能启动；已确认没有子进程").await?;
            return Ok(());
        }
    };
    let pid = child
        .id()
        .ok_or_else(|| Error::Unavailable("API 子进程没有进程标识".into()))?;
    let channel_result = async {
        let identity = process_identity(pid).await?;
        let owner = epoch.to_string(); let id = run.id.clone();
        client.call(move |store| store.runtime_child_started(&owner, &id, pid, &identity)).await?;
        let owner = epoch.to_string(); let id = run.id.clone();
        let (binding, scope, description, task) = client.call(move |store| {
            let binding = store.bind_member(&owner, &id)?;
            let scope = store.member_scope(&binding)?;
            let description = store.member_description(&binding)?;
            let task = store.task(&scope.task_id)?;
            Ok((binding, scope, description, task))
        }).await?;
        let skill = description["skill"].as_str().ok_or_else(|| Error::Invalid("成员 Skill 缺失".into()))?;
        let (native, ledger) = preparation.context.directories(path);
        let start = json!({"workerId":run.worker_id,"contextId":preparation.context.id,
            "configurationId":run.configuration_id,"purposeFamily":preparation.context.purpose_family,
            "contextDirectory":native,"ledgerDirectory":ledger,"resume":preparation.context.used,
            "goal":task.goal,"input":json!({"message":work.message,"taskRevision":task.revision,"humanWorkerId":task.team_snapshot.acceptor}).to_string(),
            "skill":skill,"tools":description["tools"],"connection":preparation.connection});
        let (control, stop) = tokio::sync::watch::channel(false);
        let watcher_client = client.clone(); let owner = epoch.to_string(); let id = run.id.clone();
        let watcher = tokio::spawn(async move {
            loop {
                let owner = owner.clone(); let id = id.clone();
                let must_stop = watcher_client.call(move |store| {
                    if store.runtime_should_stop(&owner)? { return Ok(true); }
                    let run = store.run(&id)?;
                    Ok(crate::runs::check_run_authority(&store.connection, &run).is_err())
                }).await.unwrap_or(true);
                if must_stop { let _ = control.send(true); break; }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        });
        let result = crate::channel::serve(child.stdout.take().expect("piped stdout"), child.stdin.take().expect("piped stdin"),
            client.clone(), binding, ChannelConfiguration {scope, capabilities:Capabilities::api(skill), start}, stop).await;
        watcher.abort(); let _ = watcher.await;
        result
    }.await;
    // Only this Child handle is killed. Never signal a recorded PID from a past
    // service epoch, which could already belong to another process.
    if channel_result.is_err() {
        child.start_kill()?;
    }
    match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(status) => {
            status?;
        }
        Err(_) => {
            child.kill().await?;
            child.wait().await?;
        }
    }
    if !group_absent(pid).await? {
        return Err(Error::Unavailable(
            "API 子进程组仍有资源，保留 unknown".into(),
        ));
    }
    let owner = epoch.to_string();
    let id = run.id.clone();
    let terminal = channel_result.ok();
    let reason = if terminal.is_some() {
        "API 接入已停止并回收，成员操作已排空"
    } else {
        "API 接入失败；子进程已回收，成员操作已排空"
    };
    client.call(move |store| {
        crate::runs::service(&store.connection, &owner, false)?;
        store.connection.execute("UPDATE api_launches SET data=json_set(data,'$.terminal',json(?2),'$.adapterStopped',json('true')) WHERE run_id=?1",
            rusqlite::params![id, serde_json::to_string(&terminal)?])?;
        Ok(())
    }).await?;
    finish(client, epoch, run, reason).await
}

async fn process_table() -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("/bin/ps")
            .args(["-axo", "pid=,pgid=,lstart="])
            .env("LC_ALL", "C")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| Error::Unavailable("进程资源核对超时".into()))??;
    if !output.status.success() {
        return Err(Error::Unavailable("无法核对 API 进程资源".into()));
    }
    String::from_utf8(output.stdout).map_err(|_| Error::Unavailable("进程资源输出无效".into()))
}
async fn process_identity(pid: u32) -> Result<String> {
    let table = process_table().await?;
    let line = table
        .lines()
        .find(|line| {
            line.split_whitespace()
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                == Some(pid)
        })
        .ok_or_else(|| Error::Unavailable("API 子进程已消失".into()))?;
    Ok(crate::content::digest(line.trim().as_bytes()))
}
pub(crate) async fn group_absent(pid: u32) -> Result<bool> {
    let table = process_table().await?;
    Ok(!table.lines().any(|line| {
        line.split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u32>().ok())
            == Some(pid)
    }))
}

/// An interrupted API adapter normally exits when its private pipe closes. We
/// release only records whose original process group is actually absent. Alive
/// or unrecorded children remain unknown; no age/PID heuristic authorizes kill.
pub(crate) async fn recover(client: &DatabaseClient, epoch: &str) -> Result<()> {
    let runs = client.call(|store| {
        let mut stmt = store.connection.prepare("SELECT r.data,COALESCE(json_extract(a.data,'$.adapterStopped'),0) FROM runs r JOIN api_launches a ON a.run_id=r.id WHERE r.state='unknown'")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?)))?.collect::<std::result::Result<Vec<_>,_>>()?;
        rows.into_iter().map(|(r,stopped)| Ok((serde_json::from_str::<Run>(&r)?,stopped))).collect::<Result<Vec<_>>>()
    }).await?;
    for (run, observed) in runs {
        let absent = match run.pid {
            Some(pid) => group_absent(pid).await?,
            None => false,
        };
        if observed || absent {
            let owner = epoch.to_string();
            let id = run.id.clone();
            client.call(move |store| {
                crate::runs::service(&store.connection, &owner, false)?;
                store.connection.execute("UPDATE api_launches SET data=json_set(data,'$.adapterStopped',json('true'),'$.recovered',json('true')) WHERE run_id=?1", [id])?;
                Ok(())
            }).await?;
            match finish(
                client,
                epoch,
                &run,
                "核对确认 API 接入停止；已完成旧工具操作与检查核对",
            )
            .await
            {
                Ok(()) => {}
                Err(Error::Database(e)) => return Err(Error::Database(e)),
                // Missing candidate data or unresolved checks keep ownership.
                Err(_) => {}
            }
        }
    }
    Ok(())
}
