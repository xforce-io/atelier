use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    Human,
    Agent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Worker {
    pub execution_config: Option<String>,
    pub id: String,
    pub kind: WorkerKind,
    pub name: String,
    pub description: String,
    pub revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
pub enum Permission {
    #[serde(rename = "team.manage")]
    #[value(name = "team.manage")]
    Manage,
    #[serde(rename = "task.arrange")]
    #[value(name = "task.arrange")]
    Arrange,
    #[serde(rename = "task.execute")]
    #[value(name = "task.execute")]
    Execute,
    #[serde(rename = "task.verify")]
    #[value(name = "task.verify")]
    Verify,
    #[serde(rename = "task.deploy")]
    #[value(name = "task.deploy")]
    Deploy,
    #[serde(rename = "task.handoff")]
    #[value(name = "task.handoff")]
    Handoff,
    #[serde(rename = "task.communicate")]
    #[value(name = "task.communicate")]
    Communicate,
    #[serde(rename = "task.accept")]
    #[value(name = "task.accept")]
    Accept,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub members: Vec<String>,
    pub leader: String,
    pub executor: Option<String>,
    pub verifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployer: Option<String>,
    pub acceptor: String,
    pub grants: BTreeMap<String, Vec<Permission>>,
    pub revision: u64,
    pub authorization_revision: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    #[serde(default)]
    pub code_input: Option<String>,
    #[serde(default)]
    pub verification_profile: Option<String>,
    pub inputs: String,
    pub delivery: String,
    pub verification: String,
    pub max_runs: u32,
    pub max_messages: u32,
    pub max_reworks: u32,
}

impl Default for Contract {
    fn default() -> Self {
        Self {
            code_input: None,
            verification_profile: None,
            inputs: String::new(),
            delivery: String::new(),
            verification: String::new(),
            max_runs: 20,
            max_messages: 100,
            max_reworks: 2,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContractPatch {
    pub code_input: Option<String>,
    pub verification_profile: Option<String>,
    pub inputs: Option<String>,
    pub delivery: Option<String>,
    pub verification: Option<String>,
    pub max_runs: Option<u32>,
    pub max_messages: Option<u32>,
    pub max_reworks: Option<u32>,
}

impl ContractPatch {
    pub fn merge(&self, old: &Contract) -> Contract {
        Contract {
            code_input: self.code_input.clone().or_else(|| old.code_input.clone()),
            verification_profile: self
                .verification_profile
                .clone()
                .or_else(|| old.verification_profile.clone()),
            inputs: self.inputs.clone().unwrap_or_else(|| old.inputs.clone()),
            delivery: self
                .delivery
                .clone()
                .unwrap_or_else(|| old.delivery.clone()),
            verification: self
                .verification
                .clone()
                .unwrap_or_else(|| old.verification.clone()),
            max_runs: self.max_runs.unwrap_or(old.max_runs),
            max_messages: self.max_messages.unwrap_or(old.max_messages),
            max_reworks: self.max_reworks.unwrap_or(old.max_reworks),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamPatch {
    pub id: String,
    pub revision: u64,
    pub name: String,
    pub members: Vec<String>,
    pub leader: String,
    pub executor: Option<String>,
    pub verifier: Option<String>,
    #[serde(default)]
    pub deployer: Option<String>,
    pub grants: BTreeMap<String, Vec<Permission>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    #[serde(default)]
    pub current_artifact: Option<String>,
    pub id: String,
    pub goal: String,
    pub team_id: String,
    pub team_snapshot: Team,
    pub worker_snapshots: BTreeMap<String, Worker>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deploy: Option<DeployRecord>,
    pub cancellation_requested: bool,
    pub outcome: Option<String>,
    pub owner: Option<String>,
    pub revision: u64,
    pub contract: Contract,
    pub messages_used: u32,
    pub runs_used: u32,
    pub reworks_used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub authority_revoked: bool,
    pub id: String,
    pub task_id: String,
    pub worker_id: String,
    pub delivery_id: String,
    pub epoch: String,
    pub purpose: String,
    pub state: String,
    pub task_revision: u64,
    pub configuration_id: String,
    pub authorization_revision: u64,
    pub permissions: Vec<Permission>,
    pub launch_started: bool,
    pub stop_requested: bool,
    pub stop_reason: Option<String>,
    pub pid: Option<u32>,
    pub process_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeployRecord {
    pub state: String,
    pub acceptance_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum DeployResult {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum IntakeDecision {
    Accept,
    Wait,
    Decline,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OperationReference {
    pub actor: String,
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub recovery: Option<crate::recovery::RecoveryBasis>,
    pub acceptance: Option<crate::acceptance::AcceptanceBasis>,
    pub id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub effective_revision: u64,
    pub requester: String,
    pub handler: String,
    pub kind: String,
    pub question: String,
    pub impact: String,
    pub options: Vec<String>,
    pub revision: u64,
    pub state: String,
    pub answer: Option<String>,
    pub blocked_reason: Option<String>,
    pub reason: Option<String>,
    pub operation: Option<OperationReference>,
    pub request_delivery: String,
    pub response_delivery: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DecisionResolution {
    Operation { reference: OperationReference },
    NoChange { reason: String },
    Blocked { reason: String },
}

/// Mutating operations available to the trusted local management entry.
/// Member tools will require a separate, channel-bound identity entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum Command {
    RecoveryApply {
        id: String,
        revision: u64,
    },
    MailboxRetry {
        id: String,
        revision: u64,
        reason: String,
    },
    AcceptanceRequest {
        task_id: String,
        revision: u64,
        artifact_id: String,
        verification_id: String,
        summary: String,
    },
    AcceptanceDecide {
        task_id: String,
        revision: u64,
        request_id: String,
        request_revision: u64,
        accept: bool,
        reason: String,
        decision_ref: Option<String>,
    },
    BlockerResolve {
        id: String,
        revision: u64,
        task_revision: u64,
        evidence: String,
    },
    ConnectionTest {
        id: String,
        revision: u64,
        version: Option<String>,
        worker: Option<String>,
    },
    ConnectionPrepare {
        id: String,
        revision: u64,
        version: Option<String>,
        worker: String,
    },
    ConnectionLogin {
        id: String,
        revision: u64,
        version: Option<String>,
        worker: String,
    },
    TaskVerify {
        verification_id: Option<String>,
        blocker_id: Option<String>,
        id: String,
        revision: u64,
        artifact_id: String,
        instruction: String,
    },
    TaskRework {
        id: String,
        revision: u64,
        reason: crate::rework::ReworkReason,
        instruction: String,
    },
    TaskExecute {
        id: String,
        revision: u64,
        instruction: String,
    },
    DeployReport {
        id: String,
        revision: u64,
        result: DeployResult,
        reason: String,
    },
    CredentialSet {
        id: String,
        revision: u64,
        version: Option<String>,
    },
    CredentialClear {
        id: String,
        revision: u64,
        version: Option<String>,
    },
    ConnectionCreate {
        name: String,
        specification: crate::connection::ConnectionSpec,
    },
    ConnectionUpdate {
        id: String,
        revision: u64,
        name: Option<String>,
        specification: Option<crate::connection::ConnectionSpec>,
    },
    DecisionRequest {
        task_id: String,
        revision: u64,
        handler: String,
        question: String,
        impact: String,
        options: Vec<String>,
    },
    DecisionRespond {
        id: String,
        revision: u64,
        answer: String,
    },
    DecisionRecord {
        id: String,
        revision: u64,
        resolution: DecisionResolution,
    },
    InputImport {
        repository: String,
        commit: String,
    },
    ProfileImport {
        specification: crate::profile::VerificationProfile,
    },
    WorkerCreate {
        connection: Option<String>,
        name: String,
        description: String,
    },
    WorkerUpdate {
        connection: Option<String>,
        clear_connection: bool,
        id: String,
        revision: u64,
        name: Option<String>,
        description: Option<String>,
    },
    TeamCreate {
        team: Team,
    },
    TeamUpdate {
        patch: TeamPatch,
    },
    PermissionsUpdate {
        #[serde(default)]
        decision_id: Option<String>,
        team_id: String,
        revision: u64,
        grant: BTreeMap<String, Vec<Permission>>,
        revoke: BTreeMap<String, Vec<Permission>>,
    },
    TaskCreate {
        team_id: String,
        goal: String,
    },
    TaskUpdate {
        #[serde(default)]
        decision_id: Option<String>,
        id: String,
        revision: u64,
        goal: Option<String>,
        contract: ContractPatch,
        refresh_team: bool,
    },
    Intake {
        id: String,
        revision: u64,
        decision: IntakeDecision,
        reason: String,
    },
    MessageSend {
        task_id: String,
        recipient: String,
        kind: String,
        body: String,
        reply_to: Option<String>,
    },
    MailboxRespond {
        id: String,
        revision: u64,
        reason: String,
    },
    TaskCancel {
        id: String,
        revision: u64,
        reason: String,
    },
}
