//! Development integration harness. Uses a real API and production adapter;
//! not a CLI/Skill product acceptance run or a team scheduler.
use atelier::{
    Result,
    channel::{Capabilities, ChannelConfiguration},
    connection::{ApiProtocol, ConnectionSpec},
    database::Database,
    model::*,
    store::Store,
};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

fn required(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| atelier::Error::Unavailable(format!("缺少 {name}")))
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    match runtime.block_on(run()) {
        Ok(result) => println!("{}", result),
        Err(error) => {
            eprintln!("API integration failed: {}", error.code());
            std::process::exit(1);
        }
    }
}
async fn run() -> Result<Value> {
    let directory = PathBuf::from(required("ATELIER_SMOKE_WORKSPACE")?);
    let model = required("VOLCENGINE_MODEL")?;
    let base_url = required("VOLCENGINE_API_BASE")?;
    let api_key = required("VOLCENGINE_TOKEN")?;
    let init = Store::init(&directory, "公开合成验收输入的测试人")?;
    let human = init["self"]["id"].as_str().unwrap().to_string();
    let mut store = Store::open(&directory)?;
    let connection = store.execute(
        "connection",
        &Command::ConnectionCreate {
            name: "真实 API 集成检查".into(),
            specification: ConnectionSpec::Api {
                protocol: ApiProtocol::OpenaiChatCompletions,
                model: model.clone(),
                base_url: Some(base_url.clone()),
            },
        },
    )?;
    let connection_id = connection["connection"]["id"].as_str().unwrap();
    let mut workers = Vec::new();
    for (id, name) in [
        ("leader", "团队负责人"),
        ("executor", "执行成员"),
        ("verifier", "独立检验成员"),
    ] {
        workers.push(store.execute(
            id,
            &Command::WorkerCreate {
                name: name.into(),
                description: "处理公开合成的离线井字棋任务".into(),
                connection: Some(connection_id.into()),
            },
        )?);
    }
    let leader = workers[0]["id"].as_str().unwrap().to_string();
    let executor = workers[1]["id"].as_str().unwrap().to_string();
    let verifier = workers[2]["id"].as_str().unwrap().to_string();
    let configuration = workers[0]["execution_config"].as_str().unwrap().to_string();
    let team: Team = serde_json::from_value(store.execute(
        "team",
        &Command::TeamCreate {
            team: Team {
                id: String::new(),
                name: "真实 API 集成团队".into(),
                leader: leader.clone(),
                executor: Some(executor.clone()),
                verifier: Some(verifier.clone()),
                acceptor: human.clone(),
                members: vec![human.clone(), leader.clone(), executor, verifier],
                grants: BTreeMap::from([(
                    leader.clone(),
                    vec![Permission::Arrange, Permission::Communicate],
                )]),
                revision: 1,
                authorization_revision: 1,
            },
        },
    )?)?;
    let created=store.execute("task",&Command::TaskCreate {team_id:team.id,goal:"开发一个离线双人井字棋网页。当前仅请负责人澄清平局后如何继续：重开本局还是累计比分。规则由本人决定，先等待正式答复，不要擅自确定；本轮不承接、不编写代码、不声称交付。".into()})?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock = options.open(directory.join("runtime.lock"))?;
    lock.try_lock_exclusive()?;
    let epoch = uuid::Uuid::new_v4().to_string();
    store.runtime_register(&epoch, std::process::id())?;
    let run = store.runtime_claim(
        &epoch,
        created["delivery"]["deliveryId"].as_str().unwrap(),
        &configuration,
    )?;
    let context_id = uuid::Uuid::new_v4().to_string();
    let context = directory.join("api-context");
    let ledger = directory.join("api-ledger");
    for path in [&context, &ledger] {
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(path)?;
    }
    store.runtime_begin_launch(&epoch, &run.id)?;
    let entry = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("adapters/milkie/dist/src/main.js");
    let mut child = tokio::process::Command::new("node")
        .arg(entry)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| atelier::Error::Unavailable("子进程不存在".into()))?;
    store.runtime_child_started(
        &epoch,
        &run.id,
        pid,
        &format!(
            "owned-api-child-{pid}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ),
    )?;
    let binding = store.bind_member(&epoch, &run.id)?;
    let description = store.member_description(&binding)?;
    let skill = description["skill"].as_str().unwrap();
    let scope = store.member_scope(&binding)?;
    let task = store.task(&run.task_id)?;
    let input=json!({"message":store.mailbox(Some(&leader))?[0]["message"],"task":task.goal,"humanWorkerId":human}).to_string();
    let database = Database::open(directory.clone(), 8).await?;
    let (control, stop) = tokio::sync::watch::channel(false);
    let result=tokio::time::timeout(Duration::from_secs(180),atelier::channel::serve(child.stdout.take().unwrap(),child.stdin.take().unwrap(),database.client(),binding,ChannelConfiguration {scope,capabilities:Capabilities::api(skill),start:json!({"workerId":leader,"contextId":context_id,"configurationId":configuration,"purposeFamily":"coordinate","contextDirectory":context,"ledgerDirectory":ledger,"resume":false,"goal":task.goal,"input":input,"skill":skill,"tools":description["tools"],"connection":{"protocol":"openai-chat-completions","model":model,"baseUrl":base_url,"apiKey":api_key}})},stop)).await;
    drop(control);
    let terminal = result.ok().and_then(std::result::Result::ok);
    // Only this harness's owned API child exists: no container or proxy was
    // installed. Stop and reap it, drain database jobs, then record observation.
    if terminal.is_none() {
        let _ = child.kill().await;
    }
    let exit = match tokio::time::timeout(Duration::from_secs(10), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            child.kill().await?;
            child.wait().await?
        }
    };
    database.close().await?;
    store.runtime_run_observed_stopped(
        &epoch,
        &run.id,
        "集成入口已回收其实际 API 子进程并排空数据库，无容器或代理",
    )?;
    store.runtime_stopped(&epoch)?;
    let mailbox = store.mailbox(Some(&leader))?;
    let decisions = store.decisions(&run.task_id)?;
    let human_mailbox = store.mailbox(Some(&human))?;
    Ok(
        json!({"kind":"real_api_integration","productAcceptance":false,"model":model,"workspace":directory,"runId":run.id,"taskId":run.task_id,"childExitedSuccessfully":exit.success(),"terminal":terminal,"taskState":store.task(&run.task_id)?.state,"receiptStatus":mailbox[0]["status"],"handlingResult":mailbox[0]["handlingResult"],"decisions":decisions,"humanMailbox":human_mailbox}),
    )
}
