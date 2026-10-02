use atelier::{Envelope, Error, Result, model::*, store::Store};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(version, about = "Atelier 持久团队核心（开发中，完整团队尚未交付）")]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    workspace: PathBuf,
    #[arg(long, global = true)]
    json: bool,
    /// 写操作稳定标识；超时后用相同 ID 查询或重试。
    #[arg(long, global = true)]
    request_id: Option<String>,
    #[command(subcommand)]
    command: Top,
}

#[derive(Subcommand)]
enum Top {
    #[command(subcommand)]
    Skill(SkillCommand),
    #[command(subcommand)]
    Verification(VerificationCommand),
    #[command(subcommand)]
    Check(CheckCommand),
    #[command(subcommand)]
    Handoff(HandoffCommand),
    #[command(subcommand)]
    Artifact(ArtifactCommand),
    #[command(subcommand)]
    Run(RunCommand),
    #[command(subcommand)]
    Connection(ConnectionCommand),
    #[command(subcommand)]
    Input(InputCommand),
    #[command(subcommand)]
    Profile(ProfileCommand),
    #[command(subcommand)]
    Sample(SampleCommand),
    #[command(subcommand)]
    Runtime(RuntimeCommand),
    #[command(hide = true)]
    RuntimeServe {
        #[arg(long)]
        epoch: String,
    },
    #[command(subcommand)]
    Workspace(Workspace),
    #[command(subcommand)]
    Worker(WorkerCommand),
    #[command(subcommand)]
    Team(TeamCommand),
    #[command(subcommand)]
    Task(TaskCommand),
    #[command(subcommand)]
    Mailbox(MailboxCommand),
    #[command(subcommand)]
    Message(MessageCommand),
    #[command(subcommand)]
    Request(RequestCommand),
    Doctor,
}

#[derive(Subcommand)]
enum SkillCommand {
    Install {
        /// 安装最小宿主入口到新目录；相同内容可重复，不覆盖修改。
        #[arg(long)]
        destination: PathBuf,
    },
    Describe {
        #[arg(long)]
        protocol: u8,
        #[arg(long, conflicts_with = "task")]
        team: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
}

#[derive(Subcommand)]
enum ArtifactCommand {
    Show {
        id: String,
    },
    Export {
        id: String,
        #[arg(long)]
        destination: std::path::PathBuf,
    },
}
#[derive(Subcommand)]
enum HandoffCommand {
    Show { id: String },
}
#[derive(Subcommand)]
enum VerificationCommand {
    Show { id: String },
}
#[derive(Subcommand)]
enum CheckCommand {
    Show { id: String },
}

#[derive(Subcommand)]
enum RunCommand {
    Show { id: String },
}

#[derive(Subcommand)]
enum InputCommand {
    Import {
        #[arg(long)]
        repository: PathBuf,
        #[arg(long)]
        commit: String,
    },
    List,
    Show {
        id: String,
    },
}
#[derive(Subcommand)]
enum ProfileCommand {
    Import {
        #[arg(long)]
        file: PathBuf,
    },
    List,
    Show {
        id: String,
    },
}

#[derive(Subcommand)]
enum ConnectionCommand {
    Test {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        worker: Option<String>,
    },
    #[command(subcommand)]
    Credential(CredentialCommand),
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        file: PathBuf,
    },
    Update {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    List,
    Show {
        id: String,
    },
    Version {
        id: String,
    },
}

#[derive(Subcommand)]
enum CredentialCommand {
    Clear {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        version: Option<String>,
    },
    Set {
        id: String,
        /// 可显式修复已冻结的旧连接版本；不改变连接配置。
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        revision: u64,
        /// 只从管道读取秘密；不接受凭据 argv 或回显式终端输入。
        #[arg(long, required = true)]
        stdin: bool,
    },
}

#[derive(Subcommand)]
enum RuntimeCommand {
    Start,
    Status,
    Stop,
    /// 独占核对旧资源，不领取投递；服务持锁时拒绝。
    Reconcile,
}

#[derive(Subcommand)]
enum SampleCommand {
    Prepare {
        #[arg(long)]
        destination: PathBuf,
    },
}

#[derive(Subcommand)]
enum Workspace {
    Init {
        #[arg(long)]
        name: String,
    },
    Show,
}
#[derive(Subcommand)]
enum WorkerCommand {
    Create {
        #[arg(long)]
        connection: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    Update {
        #[arg(long, conflicts_with = "clear_connection")]
        connection: Option<String>,
        #[arg(long)]
        clear_connection: bool,
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    List,
    Show {
        id: String,
    },
}
#[derive(Args)]
struct TeamArgs {
    #[arg(long)]
    name: String,
    #[arg(long, value_delimiter = ',', required = true)]
    members: Vec<String>,
    #[arg(long)]
    leader: String,
    #[arg(long)]
    executor: Option<String>,
    #[arg(long)]
    verifier: Option<String>,
    /// 显式增量授权，格式 WORKER_ID:task.execute；可重复。
    #[arg(long)]
    grant: Vec<String>,
}
#[derive(Subcommand)]
enum TeamCommand {
    #[command(subcommand)]
    Permissions(PermissionsCommand),
    Create {
        #[command(flatten)]
        fields: TeamArgs,
    },
    Update {
        id: String,
        #[arg(long)]
        revision: u64,
        #[command(flatten)]
        fields: TeamArgs,
    },
    List,
    Show {
        id: String,
    },
}
#[derive(Subcommand)]
enum PermissionsCommand {
    Update {
        #[arg(long)]
        decision: Option<String>,
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        grant: Vec<String>,
        #[arg(long)]
        revoke: Vec<String>,
    },
}
#[derive(Subcommand)]
enum BlockerCommand {
    Show {
        id: String,
    },
    List {
        #[arg(long)]
        task: String,
    },
    Resolve {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        task_revision: u64,
        #[arg(long)]
        evidence: String,
    },
}
#[derive(Subcommand)]
enum AcceptanceCommand {
    Request {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        artifact: String,
        #[arg(long)]
        verification: String,
        #[arg(long)]
        summary: String,
    },
    Show {
        id: String,
    },
}
#[derive(Args)]
struct AcceptanceDecisionArgs {
    id: String,
    #[arg(long)]
    revision: u64,
    #[arg(long)]
    request: String,
    #[arg(long)]
    request_revision: u64,
    #[arg(long)]
    reason: String,
    #[arg(long)]
    decision_ref: Option<String>,
}
impl AcceptanceDecisionArgs {
    fn command(self, accept: bool) -> Command {
        Command::AcceptanceDecide {
            task_id: self.id,
            revision: self.revision,
            request_id: self.request,
            request_revision: self.request_revision,
            accept,
            reason: self.reason,
            decision_ref: self.decision_ref,
        }
    }
}
#[derive(Subcommand)]
enum TaskCommand {
    #[command(subcommand)]
    Recovery(RecoveryCommand),
    #[command(subcommand)]
    Acceptance(AcceptanceCommand),
    Accept(AcceptanceDecisionArgs),
    Reject(AcceptanceDecisionArgs),
    #[command(subcommand)]
    Blocker(BlockerCommand),
    #[command(group(clap::ArgGroup::new("rework_reason").required(true).args(["verification", "failed_run", "blocker", "rejection"])))]
    Rework {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        verification: Option<String>,
        #[arg(long)]
        failed_run: Option<String>,
        #[arg(long)]
        blocker: Option<String>,
        #[arg(long)]
        rejection: Option<String>,
        #[arg(long)]
        instruction: String,
    },
    Verify {
        #[arg(long, conflicts_with = "blocker")]
        inconclusive: Option<String>,
        #[arg(long)]
        blocker: Option<String>,
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        artifact: String,
        #[arg(long)]
        instruction: String,
    },
    Execute {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        instruction: String,
    },
    #[command(subcommand)]
    Decision(DecisionCommand),
    Create {
        #[arg(long)]
        team: String,
        #[arg(long)]
        goal: String,
    },
    Update {
        #[arg(long)]
        decision: Option<String>,
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        goal: Option<String>,
        #[arg(long)]
        inputs: Option<String>,
        #[arg(long)]
        delivery: Option<String>,
        #[arg(long)]
        verification: Option<String>,
        #[arg(long)]
        code_input: Option<String>,
        #[arg(long)]
        verification_profile: Option<String>,
        #[arg(long)]
        max_runs: Option<u32>,
        #[arg(long)]
        max_messages: Option<u32>,
        #[arg(long)]
        max_reworks: Option<u32>,
        #[arg(long)]
        refresh_team: bool,
    },
    Intake {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long, value_enum)]
        decision: IntakeDecision,
        #[arg(long)]
        reason: String,
    },
    Cancel {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        reason: String,
    },
    List,
    Show {
        id: String,
    },
}
#[derive(Subcommand)]
enum RecoveryCommand {
    Apply {
        id: String,
        #[arg(long)]
        revision: u64,
    },
}
#[derive(Subcommand)]
enum DecisionCommand {
    Request {
        #[arg(long)]
        task: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        handler: String,
        #[arg(long)]
        question: String,
        #[arg(long)]
        impact: String,
        #[arg(long = "option")]
        options: Vec<String>,
    },
    Respond {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        answer: String,
    },
    Record {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long, requires="operation_actor", conflicts_with_all=["no_change","blocked"])]
        operation_request: Option<String>,
        #[arg(long, requires = "operation_request")]
        operation_actor: Option<String>,
        #[arg(long, conflicts_with = "blocked")]
        no_change: Option<String>,
        #[arg(long)]
        blocked: Option<String>,
    },
    List {
        #[arg(long)]
        task: String,
    },
    Show {
        id: String,
    },
}

#[derive(Subcommand)]
enum MailboxCommand {
    Retry {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        reason: String,
    },
    List {
        #[arg(long)]
        worker: Option<String>,
    },
    Respond {
        id: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        reason: String,
    },
}
#[derive(Subcommand)]
enum MessageCommand {
    Send {
        #[arg(long)]
        task: String,
        #[arg(long)]
        recipient: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        body: String,
        #[arg(long)]
        reply_to: Option<String>,
    },
}
#[derive(Subcommand)]
enum RequestCommand {
    Show { id: String },
}

fn grants(values: Vec<String>) -> Result<BTreeMap<String, Vec<Permission>>> {
    let mut grants: BTreeMap<String, Vec<Permission>> = BTreeMap::new();
    for grant in values {
        let (worker, permission) = grant
            .split_once(':')
            .ok_or_else(|| Error::Invalid("授权格式为 WORKER_ID:task.execute".into()))?;
        let permission = Permission::from_str(permission, false).map_err(Error::Invalid)?;
        let permissions = grants.entry(worker.into()).or_default();
        if !permissions.contains(&permission) {
            permissions.push(permission);
        }
    }
    Ok(grants)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &std::path::Path) -> Result<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err(Error::Invalid("配置文件超过 64 KiB".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn run(cli: Cli) -> Result<Value> {
    match &cli.command {
        Top::Skill(SkillCommand::Install { destination }) => {
            match Store::open(&cli.workspace) {
                Ok(_) | Err(Error::NotFound(_)) => {}
                Err(error) => return Err(error),
            }
            return atelier::skill::install(destination);
        }
        Top::Skill(SkillCommand::Describe {
            protocol,
            team,
            task,
        }) => {
            return atelier::skill::describe(
                &cli.workspace,
                *protocol,
                team.as_deref(),
                task.as_deref(),
            );
        }
        Top::Sample(SampleCommand::Prepare { destination }) => {
            return atelier::sample::prepare(destination);
        }
        Top::Runtime(RuntimeCommand::Start) => return atelier::runtime::start(&cli.workspace),
        Top::Runtime(RuntimeCommand::Status) => return atelier::runtime::status(&cli.workspace),
        Top::Runtime(RuntimeCommand::Stop) => return atelier::runtime::stop(&cli.workspace),
        Top::Runtime(RuntimeCommand::Reconcile) => {
            return tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(atelier::runtime::reconcile(&cli.workspace));
        }
        Top::RuntimeServe { epoch } => {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(atelier::runtime::serve(&cli.workspace, epoch.clone()))?;
            return Ok(json!({"state":"stopped"}));
        }
        _ => {}
    }
    if let Top::Workspace(Workspace::Init { name }) = &cli.command {
        return Store::init(&cli.workspace, name);
    }
    if matches!(cli.command, Top::Doctor) {
        return match Store::open(&cli.workspace) {
            Ok(store) => Ok(
                json!({"workspace":store.workspace()?,"runtime":atelier::runtime::status(&cli.workspace)?,"execution":{"api":"supported_with_frozen_configuration_and_local_credential","agentCli":"not_implemented"},"skill":"host_install_describe_and_member_scoped_guidance"}),
            ),
            Err(Error::NotFound(_)) => Ok(
                json!({"workspace":"missing","next":"workspace init","runtime":"workspace_required","execution":"workspace_required","skill":"host_install_describe_and_member_scoped_guidance"}),
            ),
            Err(error) => Err(error),
        };
    }
    let mut store = Store::open(&cli.workspace)?;
    let command = match cli.command {
        Top::Task(TaskCommand::Recovery(RecoveryCommand::Apply { id, revision })) => {
            Command::RecoveryApply { id, revision }
        }
        Top::Task(TaskCommand::Acceptance(AcceptanceCommand::Request {
            id,
            revision,
            artifact,
            verification,
            summary,
        })) => Command::AcceptanceRequest {
            task_id: id,
            revision,
            artifact_id: artifact,
            verification_id: verification,
            summary,
        },
        Top::Task(TaskCommand::Acceptance(AcceptanceCommand::Show { id })) => {
            return Ok(json!(store.acceptance_decision(&id)?));
        }
        Top::Task(TaskCommand::Accept(args)) => args.command(true),
        Top::Task(TaskCommand::Reject(args)) => args.command(false),
        Top::Task(TaskCommand::Blocker(BlockerCommand::Show { id })) => {
            return Ok(json!(store.blocker(&id)?));
        }
        Top::Task(TaskCommand::Blocker(BlockerCommand::List { task })) => {
            return store.blockers(&task);
        }
        Top::Task(TaskCommand::Blocker(BlockerCommand::Resolve {
            id,
            revision,
            task_revision,
            evidence,
        })) => Command::BlockerResolve {
            id,
            revision,
            task_revision,
            evidence,
        },
        Top::Verification(VerificationCommand::Show { id }) => {
            return Ok(json!(store.verification(&id)?));
        }
        Top::Check(CheckCommand::Show { id }) => return Ok(json!(store.check(&id)?)),
        Top::Handoff(HandoffCommand::Show { id }) => return Ok(json!(store.handoff(&id)?)),
        Top::Task(TaskCommand::Rework {
            id,
            revision,
            verification,
            failed_run,
            blocker,
            rejection,
            instruction,
        }) => Command::TaskRework {
            id,
            revision,
            instruction,
            reason: match (verification, failed_run, blocker, rejection) {
                (Some(id), None, None, None) => atelier::rework::ReworkReason::Verification { id },
                (None, Some(id), None, None) => atelier::rework::ReworkReason::RunFailure { id },
                (None, None, Some(id), None) => atelier::rework::ReworkReason::Blocker { id },
                (None, None, None, Some(id)) => atelier::rework::ReworkReason::Rejection { id },
                _ => return Err(Error::Invalid("返工须指定且仅指定一种原因引用".into())),
            },
        },
        Top::Task(TaskCommand::Verify {
            inconclusive,
            blocker,
            id,
            revision,
            artifact,
            instruction,
        }) => Command::TaskVerify {
            verification_id: inconclusive,
            blocker_id: blocker,
            id,
            revision,
            artifact_id: artifact,
            instruction,
        },
        Top::Artifact(ArtifactCommand::Show { id }) => return Ok(json!(store.artifact(&id)?)),
        Top::Artifact(ArtifactCommand::Export { id, destination }) => {
            return store.export_artifact(&id, &destination);
        }
        Top::Task(TaskCommand::Execute {
            id,
            revision,
            instruction,
        }) => Command::TaskExecute {
            id,
            revision,
            instruction,
        },
        Top::Workspace(Workspace::Show) => return store.workspace(),
        Top::Worker(WorkerCommand::List) => return store.list("worker"),
        Top::Input(InputCommand::List) => return store.list("input"),
        Top::Input(InputCommand::Show { id }) => return Ok(json!(store.input(&id)?)),
        Top::Input(InputCommand::Import { repository, commit }) => Command::InputImport {
            repository: repository
                .to_str()
                .ok_or_else(|| Error::Invalid("输入目录须为 UTF-8".into()))?
                .to_string(),
            commit,
        },
        Top::Profile(ProfileCommand::List) => return store.list("profile"),
        Top::Profile(ProfileCommand::Show { id }) => return Ok(json!(store.profile(&id)?)),
        Top::Profile(ProfileCommand::Import { file }) => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(file)?
                .take(65537)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 65536 {
                return Err(Error::Invalid("检验配置文件超过 64 KiB".into()));
            }
            Command::ProfileImport {
                specification: serde_json::from_slice(&bytes)?,
            }
        }
        Top::Worker(WorkerCommand::Show { id }) => return Ok(json!(store.worker(&id)?)),
        Top::Run(RunCommand::Show { id }) => return store.run_view(&id),
        Top::Connection(ConnectionCommand::List) => return store.list("connection"),
        Top::Connection(ConnectionCommand::Test {
            id,
            revision,
            version,
            worker,
        }) => {
            let request = cli
                .request_id
                .as_deref()
                .ok_or_else(|| Error::Invalid("连接检查必须提供 --request-id".into()))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let result = runtime.block_on(atelier::connection_probe::test(
                &mut store,
                request,
                &Command::ConnectionTest {
                    id,
                    revision,
                    version,
                    worker,
                },
            ));
            runtime.shutdown_timeout(std::time::Duration::from_millis(100));
            return result;
        }
        Top::Connection(ConnectionCommand::Credential(CredentialCommand::Clear {
            id,
            revision,
            version,
        })) => {
            let request = cli
                .request_id
                .as_deref()
                .ok_or_else(|| Error::Invalid("写操作必须提供 --request-id".into()))?;
            return store.clear_credential(request, &id, revision, version.as_deref());
        }
        Top::Connection(ConnectionCommand::Credential(CredentialCommand::Set {
            id,
            version,
            revision,
            stdin: _,
        })) => {
            use std::io::{IsTerminal, Read};
            let request = cli
                .request_id
                .as_deref()
                .ok_or_else(|| Error::Invalid("写操作必须提供 --request-id".into()))?;
            if std::io::stdin().is_terminal() {
                return Err(Error::Invalid(
                    "凭据须通过标准输入管道提供，不在终端回显输入".into(),
                ));
            }
            let mut bytes = Vec::new();
            std::io::stdin().take(16386).read_to_end(&mut bytes)?;
            if bytes.last() == Some(&b'\n') {
                bytes.pop();
                if bytes.last() == Some(&b'\r') {
                    bytes.pop();
                }
            }
            let secret = String::from_utf8(bytes)
                .map_err(|_| Error::Invalid("凭据须为 UTF-8 单行文本".into()))?;
            return store.set_credential_version(
                request,
                &id,
                revision,
                version.as_deref(),
                &secret,
            );
        }
        Top::Connection(ConnectionCommand::Show { id }) => return store.connection_view(&id),
        Top::Connection(ConnectionCommand::Version { id }) => {
            return Ok(json!(store.connection_version(&id)?));
        }
        Top::Connection(ConnectionCommand::Create { name, file }) => Command::ConnectionCreate {
            name,
            specification: read_json(&file)?,
        },
        Top::Connection(ConnectionCommand::Update {
            id,
            revision,
            name,
            file,
        }) => Command::ConnectionUpdate {
            id,
            revision,
            name,
            specification: file.as_ref().map(|path| read_json(path)).transpose()?,
        },
        Top::Worker(WorkerCommand::Create {
            name,
            description,
            connection,
        }) => Command::WorkerCreate {
            name,
            description,
            connection,
        },
        Top::Worker(WorkerCommand::Update {
            connection,
            clear_connection,
            id,
            revision,
            name,
            description,
        }) => Command::WorkerUpdate {
            connection,
            clear_connection,
            id,
            revision,
            name,
            description,
        },
        Top::Team(TeamCommand::List) => return store.list("team"),
        Top::Team(TeamCommand::Show { id }) => return Ok(json!(store.team(&id)?)),
        Top::Team(TeamCommand::Permissions(PermissionsCommand::Update {
            decision,
            id,
            revision,
            grant,
            revoke,
        })) => Command::PermissionsUpdate {
            decision_id: decision,
            team_id: id,
            revision,
            grant: grants(grant)?,
            revoke: grants(revoke)?,
        },
        Top::Team(TeamCommand::Create { fields }) => Command::TeamCreate {
            team: Team {
                id: String::new(),
                revision: 1,
                authorization_revision: 1,
                name: fields.name,
                members: fields.members,
                leader: fields.leader,
                executor: fields.executor,
                verifier: fields.verifier,
                acceptor: store.workspace()?["self"]["id"]
                    .as_str()
                    .ok_or_else(|| Error::Invalid("本人身份缺失".into()))?
                    .to_string(),
                grants: grants(fields.grant)?,
            },
        },
        Top::Team(TeamCommand::Update {
            id,
            revision,
            fields,
        }) => Command::TeamUpdate {
            patch: TeamPatch {
                id,
                revision,
                name: fields.name,
                members: fields.members,
                leader: fields.leader,
                executor: fields.executor,
                verifier: fields.verifier,
                grants: grants(fields.grant)?,
            },
        },
        Top::Task(TaskCommand::Decision(DecisionCommand::Show { id })) => {
            return Ok(json!(store.decision(&id)?));
        }
        Top::Task(TaskCommand::Decision(DecisionCommand::List { task })) => {
            return store.decisions(&task);
        }
        Top::Task(TaskCommand::Decision(DecisionCommand::Request {
            task,
            revision,
            handler,
            question,
            impact,
            options,
        })) => Command::DecisionRequest {
            task_id: task,
            revision,
            handler,
            question,
            impact,
            options,
        },
        Top::Task(TaskCommand::Decision(DecisionCommand::Respond {
            id,
            revision,
            answer,
        })) => Command::DecisionRespond {
            id,
            revision,
            answer,
        },
        Top::Task(TaskCommand::Decision(DecisionCommand::Record {
            id,
            revision,
            operation_request,
            operation_actor,
            no_change,
            blocked,
        })) => {
            let resolution = match (operation_request, operation_actor, no_change, blocked) {
                (Some(request_id), Some(actor), None, None) => DecisionResolution::Operation {
                    reference: OperationReference { actor, request_id },
                },
                (None, None, Some(reason), None) => DecisionResolution::NoChange { reason },
                (None, None, None, Some(reason)) => DecisionResolution::Blocked { reason },
                _ => {
                    return Err(Error::Invalid(
                        "须明确指定关联操作、无需变更或落实受阻中的一种结果".into(),
                    ));
                }
            };
            Command::DecisionRecord {
                id,
                revision,
                resolution,
            }
        }
        Top::Task(TaskCommand::List) => return store.list("task"),
        Top::Task(TaskCommand::Show { id }) => return store.task_details(&id),
        Top::Task(TaskCommand::Create { team, goal }) => Command::TaskCreate {
            team_id: team,
            goal,
        },
        Top::Task(TaskCommand::Update {
            decision,
            id,
            revision,
            goal,
            inputs,
            delivery,
            verification,
            code_input,
            verification_profile,
            max_runs,
            max_messages,
            max_reworks,
            refresh_team,
        }) => Command::TaskUpdate {
            decision_id: decision,
            id,
            revision,
            goal,
            contract: ContractPatch {
                inputs,
                delivery,
                verification,
                code_input,
                verification_profile,
                max_runs,
                max_messages,
                max_reworks,
            },
            refresh_team,
        },
        Top::Task(TaskCommand::Intake {
            id,
            revision,
            decision,
            reason,
        }) => Command::Intake {
            id,
            revision,
            decision,
            reason,
        },
        Top::Task(TaskCommand::Cancel {
            id,
            revision,
            reason,
        }) => Command::TaskCancel {
            id,
            revision,
            reason,
        },
        Top::Mailbox(MailboxCommand::Retry {
            id,
            revision,
            reason,
        }) => Command::MailboxRetry {
            id,
            revision,
            reason,
        },
        Top::Mailbox(MailboxCommand::List { worker }) => return store.mailbox(worker.as_deref()),
        Top::Mailbox(MailboxCommand::Respond {
            id,
            revision,
            reason,
        }) => Command::MailboxRespond {
            id,
            revision,
            reason,
        },
        Top::Message(MessageCommand::Send {
            task,
            recipient,
            kind,
            body,
            reply_to,
        }) => Command::MessageSend {
            task_id: task,
            recipient,
            kind,
            body,
            reply_to,
        },
        Top::Request(RequestCommand::Show { id }) => return store.request(&id),
        Top::Doctor
        | Top::Workspace(Workspace::Init { .. })
        | Top::Runtime(_)
        | Top::Skill(_)
        | Top::RuntimeServe { .. }
        | Top::Sample(_) => unreachable!(),
    };
    let request = cli.request_id.ok_or_else(|| {
        Error::Invalid("写操作需要 --request-id；超时后按该 ID 查询或重试".into())
    })?;
    store.execute(&request, &command)
}

fn main() -> ExitCode {
    let json_requested = std::env::args_os().any(|arg| arg == "--json");
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            let credential_input =
                error.use_stderr() && std::env::args().any(|arg| arg == "connection");
            if error.use_stderr() && json_requested {
                println!(
                    "{}",
                    json!({"version":2,"ok":false,"error":{"code":"invalid_request","message":if credential_input {"连接命令参数无效；凭据仅通过 credential set --stdin 设置，请查看 connection --help".into()} else {error.to_string()}}})
                );
            } else if credential_input {
                eprintln!(
                    "连接命令参数无效；凭据仅通过 credential set --stdin 设置，请查看 connection --help"
                );
            } else {
                let _ = error.print();
            }
            return if error.use_stderr() {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    let json_output = cli.json;
    match run(cli) {
        Ok(value) => {
            let result = Envelope {
                version: 2,
                ok: true,
                data: value,
            };
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string(&result).expect("JSON value serialization")
                );
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&result.data).expect("JSON value serialization")
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            if json_output {
                println!(
                    "{}",
                    json!({"version":2,"ok":false,"error":{"code":error.code(),"message":error.to_string()}})
                );
            } else {
                eprintln!("{}：{error}", error.code());
            }
            ExitCode::from(1)
        }
    }
}
