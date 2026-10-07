//! Trusted service boundary for member tools. Bindings are created from owned
//! Runs, never deserialized from a model request or exposed by the management CLI.
use crate::{
    Error, Result,
    model::{
        Command, ContractPatch, DecisionRequest, DecisionResolution, IntakeDecision,
        OperationReference, Permission, Run, Task, Team,
    },
    runs::{check_run_authority, service},
    store::{Store, apply, load, participants, require, text},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// A 256 KiB UTF-8 file can require six JSON bytes per content byte.
// Keep encoding overhead separate from the unchanged domain/result limits.
pub(crate) const MAX_TOOL_REQUEST: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct MemberBinding {
    epoch: String,
    run_id: String,
}

/// Transport identity is generated and persisted by the trusted adapter before
/// forwarding. The model chooses only an installed tool's input.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolOperation {
    pub operation_id: String,
    pub originating_run_id: String,
    pub tool_call_id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileOperation {
    pub delivery_id: String,
    pub operation: ToolOperation,
}

#[derive(Deserialize)]
#[serde(
    tag = "name",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum MemberCommand {
    ListFiles(crate::candidate::ListFiles),
    ReadFile(crate::candidate::ReadFile),
    WriteFile(crate::candidate::WriteFile),
    DeleteFile(crate::candidate::FilePath),
    RunCheck(NamedCheck),
    CheckRead(DecisionId),
    VerificationSubmit(SubmitVerification),
    HandoffRead(DecisionId),
    HandoffOffer(OfferHandoff),
    HandoffRespond(RespondHandoff),
    ArtifactSubmit(SubmitArtifact),
    TaskRead(Empty),
    MailboxRetry(RetryDelivery),
    AcceptanceRequest(RequestAcceptance),
    TaskReportBlocker(ReportBlocker),
    BlockerResolve(ResolveBlocker),
    MessageSend(SendMessage),
    MessageRespond(crate::disposition::Disposition),
    DecisionRequest(RequestDecision),
    DecisionRead(DecisionId),
    DecisionRespond(AnswerDecision),
    DecisionRecord(RecordDecision),
    TaskUpdate(UpdateTask),
    TaskIntake(Intake),
    TaskArrange(Arrange),
    HostExec(HostExecInput),
    DeployVerify(VerifyInput),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryDelivery {
    id: String,
    revision: u64,
    reason: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestAcceptance {
    revision: u64,
    artifact_id: String,
    verification_id: String,
    summary: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportBlocker {
    handler: String,
    reason: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResolveBlocker {
    id: String,
    revision: u64,
    task_revision: u64,
    evidence: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct NamedCheck {
    check_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SubmitVerification {
    evidence_id: String,
    recommendation: String,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SubmitArtifact {
    summary: String,
    handoff: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Arrange {
    revision: u64,
    action: ArrangeAction,
    instruction: String,
    #[serde(default)]
    artifact_id: Option<String>,
    reason: Option<crate::rework::ReworkReason>,
    blocker_id: Option<String>,
    verification_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArrangeAction {
    Rework,
    Execute,
    Verify,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OfferHandoff {
    revision: u64,
    artifact_id: String,
    instruction: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RespondHandoff {
    id: String,
    revision: u64,
    accept: bool,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionId {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerDecision {
    id: String,
    revision: u64,
    answer: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordDecision {
    id: String,
    revision: u64,
    resolution: DecisionResolution,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateTask {
    revision: u64,
    goal: Option<String>,
    inputs: Option<String>,
    delivery: Option<String>,
    verification: Option<String>,
    decision_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HostExecInput {
    argv: Vec<String>,
    cwd: Option<String>,
    timeout_seconds: Option<u32>,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerifyInput {
    revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Intake {
    revision: u64,
    decision: IntakeDecision,
    reason: String,
}

pub(crate) struct IntakePreparation {
    workspace: std::path::PathBuf,
    input: crate::content::GitInput,
    task_id: String,
    revision: u64,
}
pub(crate) struct ValidatedIntake {
    input_id: String,
    task_id: String,
    revision: u64,
}
impl IntakePreparation {
    pub(crate) fn validate(self) -> Result<ValidatedIntake> {
        crate::content::validate_input(&self.workspace, &self.input)?;
        Ok(ValidatedIntake {
            input_id: self.input.id,
            task_id: self.task_id,
            revision: self.revision,
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SendMessage {
    recipient: String,
    kind: String,
    body: String,
    reply_to: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestDecision {
    revision: u64,
    handler: String,
    question: String,
    impact: String,
    options: Vec<String>,
}

pub(crate) fn bound_run(db: &Connection, binding: &MemberBinding) -> Result<(Run, Task)> {
    service(db, &binding.epoch, true)?;
    let run: Run = load(db, "runs", &binding.run_id)?;
    if run.epoch != binding.epoch || run.state != "running" {
        return Err(Error::Forbidden("成员通道的 Run 已失效".into()));
    }
    check_run_authority(db, &run)?;
    let task: Task = load(db, "tasks", &run.task_id)?;
    if task.revision != run.task_revision {
        return Err(Error::Conflict("成员执行的任务依据已变化".into()));
    }
    if !run.permissions.contains(&Permission::Communicate)
        || !participants(&task).contains(&run.worker_id.as_str())
    {
        return Err(Error::Forbidden("成员不具备该任务的通信职责".into()));
    }
    let owns: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM deliveries WHERE id=?1 AND run_id=?2 AND status='claimed' AND receiver=?3)",
        params![run.delivery_id, run.id, run.worker_id], |r| r.get(0))?;
    if !owns {
        return Err(Error::Forbidden("成员通道不再持有该投递".into()));
    }
    Ok((run, task))
}

impl Store {
    /// Read-only transport recovery under the current member's authority. This
    /// is not a model tool and never calls member_effect or creates a request.
    pub fn member_reconcile(
        &self,
        binding: &MemberBinding,
        request: &ReconcileOperation,
    ) -> Result<Value> {
        let tx = self.connection.unchecked_transaction()?;
        let (mut run, task) = bound_run(&tx, binding)?;
        let operation = &request.operation;
        for id in [
            &request.delivery_id,
            &operation.operation_id,
            &operation.originating_run_id,
            &operation.tool_call_id,
        ] {
            text(id, "旧调用标识", 256)?;
        }
        let bytes = serde_json::to_vec(operation)?;
        if bytes.len() > MAX_TOOL_REQUEST {
            return Err(Error::Invalid("旧调用 JSON 超过 2 MiB".into()));
        }
        let origin: Run = load(&tx, "runs", &operation.originating_run_id)?;
        if origin.id == run.id
            || origin.state != "stopped"
            || origin.task_id != run.task_id
            || origin.worker_id != run.worker_id
            || origin.configuration_id != run.configuration_id
            || origin.delivery_id != request.delivery_id
        {
            return Err(Error::Forbidden("旧调用不属于同成员已停止的执行".into()));
        }
        let context = |id: &str| -> Result<Option<String>> {
            Ok(tx.query_row("SELECT json_extract(data,'$.contextId') FROM api_launches WHERE run_id=?1 UNION ALL SELECT json_extract(data,'$.contextId') FROM cli_resources WHERE run_id=?1",[id],|r|r.get(0)).optional()?.flatten())
        };
        let current_context = context(&run.id)?;
        if current_context.is_none() || context(&origin.id)? != current_context {
            return Err(Error::Forbidden("旧调用不属于当前原生执行上下文".into()));
        }
        let team: Team = load(&tx, "teams", &task.team_id)?;
        run.permissions
            .retain(|permission| require(&team, &run.worker_id, permission.clone()).is_ok());
        let description = crate::member_tools::describe(&run, &task)?;
        if !description["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == operation.name))
        {
            return Err(Error::Forbidden("当前成员不再具有旧调用所需权限".into()));
        }
        let fingerprint = format!("{:x}", Sha256::digest(bytes));
        let prior:Option<(String,String)>=tx.query_row("SELECT fingerprint,result FROM member_requests WHERE delivery_id=?1 AND operation_id=?2",
            params![origin.delivery_id,operation.operation_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let result = if let Some((saved, result)) = prior {
            if saved != fingerprint {
                return Err(Error::Conflict("旧调用内容与已提交记录不符".into()));
            }
            serde_json::from_str(&result)?
        } else {
            let reused:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM member_requests WHERE originating_run_id=?1 AND tool_call_id=?2)",params![origin.id,operation.tool_call_id],|r|r.get(0))?;
            if reused {
                return Err(Error::Conflict("旧 toolCallId 与 operationId 不符".into()));
            }
            json!({"ok":false,"error":{"code":"not_executed","message":"原调用未提交；核对未执行任何新操作"}})
        };
        tx.commit()?;
        Ok(result)
    }

    pub fn member_description(&self, binding: &MemberBinding) -> Result<Value> {
        let tx = self.connection.unchecked_transaction()?;
        let (mut run, task) = bound_run(&tx, binding)?;
        let team: Team = load(&tx, "teams", &task.team_id)?;
        run.permissions
            .retain(|p| crate::store::require(&team, &run.worker_id, p.clone()).is_ok());
        let result = crate::member_tools::describe(&run, &task)?;
        tx.commit()?;
        Ok(result)
    }
    pub(crate) fn prepare_member_intake(
        &self,
        binding: &MemberBinding,
        operation: &ToolOperation,
    ) -> Result<Option<IntakePreparation>> {
        let (run, task) = bound_run(&self.connection, binding)?;
        if operation.name != "task_intake" {
            return Ok(None);
        }
        let prior: bool = self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM member_requests WHERE delivery_id=?1 AND operation_id=?2)",
            params![run.delivery_id, operation.operation_id],
            |r| r.get(0),
        )?;
        if prior {
            return Ok(None);
        }
        let Ok(input) = serde_json::from_value::<Intake>(operation.input.clone()) else {
            return Ok(None);
        };
        if !matches!(input.decision, IntakeDecision::Accept) || task.revision != input.revision {
            return Ok(None);
        }
        authorize_intake(&self.connection, &run, &task)?;
        let Some(id) = task.contract.code_input else {
            return Ok(None);
        };
        Ok(Some(IntakePreparation {
            workspace: self.workspace_path.clone(),
            input: self.input(&id)?,
            task_id: task.id,
            revision: task.revision,
        }))
    }
    pub fn member_scope(&self, binding: &MemberBinding) -> Result<crate::channel::Scope> {
        let (run, _) = bound_run(&self.connection, binding)?;
        Ok(crate::channel::Scope {
            task_id: run.task_id,
            run_id: run.id,
            delivery_id: run.delivery_id,
        })
    }
    /// Only the service that owns the private child channel may call this.
    pub fn bind_member(&self, epoch: &str, run_id: &str) -> Result<MemberBinding> {
        let binding = MemberBinding {
            epoch: epoch.into(),
            run_id: run_id.into(),
        };
        bound_run(&self.connection, &binding)?;
        Ok(binding)
    }

    pub fn member_call(
        &mut self,
        binding: &MemberBinding,
        operation: &ToolOperation,
    ) -> Result<Value> {
        self.member_call_prepared(binding, operation, None, None)
    }

    pub(crate) fn member_call_prepared(
        &mut self,
        binding: &MemberBinding,
        operation: &ToolOperation,
        prepared: Option<&ValidatedIntake>,
        file: Option<&crate::candidate::Prepared>,
    ) -> Result<Value> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Authorization precedes cached results, including read-only tools.
        let (run, task) = bound_run(&tx, binding)?;
        for id in [
            &operation.operation_id,
            &operation.originating_run_id,
            &operation.tool_call_id,
        ] {
            text(id, "工具调用标识", 256)?;
        }
        let bytes = serde_json::to_vec(operation)?;
        if bytes.len() > MAX_TOOL_REQUEST {
            return Err(Error::Invalid("工具请求 JSON 超过 2 MiB".into()));
        }
        let fingerprint = format!("{:x}", Sha256::digest(bytes));
        let origin: Run = load(&tx, "runs", &operation.originating_run_id)?;
        if origin.delivery_id != run.delivery_id
            || origin.worker_id != run.worker_id
            || origin.task_id != run.task_id
            || (origin.id != run.id && origin.state != "stopped")
        {
            return Err(Error::Forbidden("工具调用来源不属于当前投递".into()));
        }
        let prior: Option<(String, String)> = tx.query_row(
            "SELECT fingerprint,result FROM member_requests WHERE delivery_id=?1 AND operation_id=?2",
            params![run.delivery_id, operation.operation_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((hash, result)) = prior {
            if hash != fingerprint {
                return Err(Error::Conflict("同 operationId 的工具请求内容不同".into()));
            }
            return Ok(serde_json::from_str(&result)?);
        }
        if origin.id != run.id {
            return Err(Error::Conflict(
                "旧 Run 的未提交调用不能补执行；先只读核对，再由当前 Run 决定新操作".into(),
            ));
        }
        let used_call: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM member_requests WHERE originating_run_id=?1 AND tool_call_id=?2)",
            params![origin.id, operation.tool_call_id], |r| r.get(0))?;
        if used_call {
            return Err(Error::Conflict("同 toolCallId 不能更换 operationId".into()));
        }
        let used: u32 = tx.query_row(
            "SELECT count(*) FROM member_requests WHERE originating_run_id=?1",
            [&origin.id],
            |r| r.get(0),
        )?;
        if used >= 100 {
            return Err(Error::Conflict("本 Run 工具调用额度耗尽".into()));
        }
        // Save business failures too: a retry must not turn an earlier rejected
        // call into a new effect after unrelated state changes.
        tx.execute_batch("SAVEPOINT member_effect;")?;
        let effect = member_effect(
            &tx,
            &run,
            &task,
            operation,
            prepared,
            file,
            &self.workspace_path,
        );
        let result = match effect {
            Ok(data) => json!({"ok":true,"data":data}),
            Err(
                error
                @ (Error::Database(_) | Error::Io(_) | Error::Unavailable(_) | Error::Json(_)),
            ) => return Err(error),
            Err(error) => {
                tx.execute_batch("ROLLBACK TO member_effect;")?;
                json!({"ok":false,"error":{"code":error.code(),"message":error.to_string()}})
            }
        };
        tx.execute_batch("RELEASE member_effect;")?;
        if serde_json::to_vec(&result)?.len() > 256 * 1024 {
            return Err(Error::Invalid("工具结果超过 256 KiB".into()));
        }
        tx.execute("INSERT INTO member_requests(delivery_id,operation_id,originating_run_id,tool_call_id,fingerprint,result) VALUES(?1,?2,?3,?4,?5,?6)",
            params![run.delivery_id,operation.operation_id,origin.id,operation.tool_call_id,fingerprint,serde_json::to_string(&result)?])?;
        tx.commit()?;
        Ok(result)
    }
}

fn member_effect(
    db: &Connection,
    run: &Run,
    task: &Task,
    operation: &ToolOperation,
    prepared: Option<&ValidatedIntake>,
    file: Option<&crate::candidate::Prepared>,
    workspace: &std::path::Path,
) -> Result<Value> {
    let command: MemberCommand =
        serde_json::from_value(json!({"name":operation.name,"input":operation.input}))
            .map_err(|_| Error::Invalid("未安装的成员工具或工具参数无效".into()))?;
    if !matches!(
        command,
        MemberCommand::TaskRead(_)
            | MemberCommand::ListFiles(_)
            | MemberCommand::ReadFile(_)
            | MemberCommand::DecisionRead(_)
            | MemberCommand::HandoffRead(_)
            | MemberCommand::CheckRead(_)
    ) && (crate::disposition::exists(db, &run.delivery_id)?
        || crate::artifact::has_submission(db, &run.id)?)
    {
        return Err(Error::Conflict(
            "该投递已保存处理结果，本轮不能提交新的业务操作".into(),
        ));
    }
    let command = match command {
        MemberCommand::MailboxRetry(input) => {
            authorize_intake(db, run, task)?;
            if crate::retry::task_for(db, &input.id)?.id != task.id {
                return Err(Error::Forbidden("不能重试其它任务投递".into()));
            }
            Command::MailboxRetry {
                id: input.id,
                revision: input.revision,
                reason: input.reason,
            }
        }
        MemberCommand::AcceptanceRequest(input) => {
            authorize_intake(db, run, task)?;
            Command::AcceptanceRequest {
                task_id: task.id.clone(),
                revision: input.revision,
                artifact_id: input.artifact_id,
                verification_id: input.verification_id,
                summary: input.summary,
            }
        }

        MemberCommand::TaskReportBlocker(input) => {
            return crate::blocker::report(db, run, task, &input.handler, &input.reason);
        }
        MemberCommand::BlockerResolve(input) => {
            if run.purpose != "coordinate" || !run.permissions.contains(&Permission::Arrange) {
                return Err(Error::Forbidden("仅获准协调 Run 可提交阻塞解决依据".into()));
            }
            let b: crate::blocker::Blocker = load(db, "blockers", &input.id)?;
            if b.task_id != task.id {
                return Err(Error::Forbidden("阻塞不属于当前任务".into()));
            }
            Command::BlockerResolve {
                id: input.id,
                revision: input.revision,
                task_revision: input.task_revision,
                evidence: input.evidence,
            }
        }

        MemberCommand::ListFiles(input) => {
            let _ = input;
            return crate::candidate::effect(db, run, task, operation, file);
        }
        MemberCommand::ReadFile(input) => {
            let _ = input;
            return crate::candidate::effect(db, run, task, operation, file);
        }
        MemberCommand::WriteFile(input) => {
            let _ = input;
            return crate::candidate::effect(db, run, task, operation, file);
        }
        MemberCommand::DeleteFile(input) => {
            let _ = input;
            return crate::candidate::effect(db, run, task, operation, file);
        }
        MemberCommand::RunCheck(input) => {
            return crate::verification::request(
                db,
                run,
                task,
                &operation.operation_id,
                &input.check_id,
            );
        }
        MemberCommand::CheckRead(input) => {
            return Ok(json!(crate::verification::read(db, run, task, &input.id)?));
        }
        MemberCommand::VerificationSubmit(input) => {
            return crate::verification::submit(
                db,
                run,
                task,
                &input.evidence_id,
                &input.recommendation,
                &input.reason,
            );
        }
        MemberCommand::HandoffRead(input) => {
            return Ok(json!(crate::handoff::read(db, run, task, &input.id)?));
        }
        MemberCommand::HandoffOffer(input) => {
            if !(run.permissions.contains(&Permission::Handoff)
                || task.team_snapshot.leader == run.worker_id
                    && run.permissions.contains(&Permission::Arrange))
            {
                return Err(Error::Forbidden("本 Run 没有交接授权".into()));
            }
            return crate::handoff::offer(
                db,
                &run.worker_id,
                &operation.operation_id,
                &task.id,
                input.revision,
                &input.artifact_id,
                &input.instruction,
            );
        }
        MemberCommand::HandoffRespond(input) => {
            return crate::handoff::respond(
                db,
                run,
                task,
                &input.id,
                input.revision,
                input.accept,
                &input.reason,
            );
        }
        MemberCommand::ArtifactSubmit(input) => {
            return crate::artifact::submit(
                db,
                run,
                task,
                &input.summary,
                input.handoff.as_deref(),
            );
        }
        MemberCommand::HostExec(input) => {
            let mut current = task.clone();
            return crate::host_work::submit(
                db,
                workspace,
                run,
                &mut current,
                crate::host_work::SubmittedCommand {
                    argv: input.argv,
                    cwd: input.cwd.unwrap_or_else(|| ".".into()),
                    timeout_seconds: input.timeout_seconds.unwrap_or(600),
                    reason: input.reason,
                },
            );
        }
        MemberCommand::DeployVerify(input) => {
            return crate::host_work::request_verification(
                db,
                &run.worker_id,
                &task.id,
                input.revision,
                Some(&run.id),
            );
        }
        MemberCommand::TaskArrange(input) => {
            authorize_intake(db, run, task)?;
            if (input.blocker_id.is_some() || input.verification_id.is_some())
                && !matches!(input.action, ArrangeAction::Verify)
            {
                return Err(Error::Invalid(
                    "blockerId/verificationId 仅用于重新检验；返工使用 reason".into(),
                ));
            }
            if input.reason.is_some() && !matches!(input.action, ArrangeAction::Rework) {
                return Err(Error::Invalid("仅返工安排可指定原因引用".into()));
            }
            match input.action {
                ArrangeAction::Rework => {
                    if input.artifact_id.is_some() {
                        return Err(Error::Invalid(
                            "返工输入由原因记录固定，不能另选产出".into(),
                        ));
                    }
                    Command::TaskRework {
                        id: task.id.clone(),
                        revision: input.revision,
                        reason: input
                            .reason
                            .ok_or_else(|| Error::Invalid("返工须指定原因引用".into()))?,
                        instruction: input.instruction,
                    }
                }
                ArrangeAction::Execute => {
                    if input.artifact_id.is_some() {
                        return Err(Error::Invalid("首次执行安排不能指定待检产出".into()));
                    }
                    Command::TaskExecute {
                        id: task.id.clone(),
                        revision: input.revision,
                        instruction: input.instruction,
                    }
                }
                ArrangeAction::Verify => Command::TaskVerify {
                    verification_id: input.verification_id,
                    blocker_id: input.blocker_id,
                    id: task.id.clone(),
                    revision: input.revision,
                    artifact_id: input
                        .artifact_id
                        .ok_or_else(|| Error::Invalid("检验安排必须指定固定产出".into()))?,
                    instruction: input.instruction,
                },
            }
        }
        MemberCommand::TaskUpdate(input) => {
            authorize_intake(db, run, task)?;
            let command = Command::TaskUpdate {
                decision_id: input.decision_id,
                id: task.id.clone(),
                revision: input.revision,
                goal: input.goal,
                contract: ContractPatch {
                    inputs: input.inputs,
                    delivery: input.delivery,
                    verification: input.verification,
                    ..Default::default()
                },
                refresh_team: false,
            };
            let cause = format!(
                "member:{:x}",
                Sha256::digest(serde_json::to_vec(&(
                    &run.delivery_id,
                    &operation.operation_id
                ))?)
            );
            let mut result = apply(db, &run.worker_id, &cause, &command, workspace)?;
            let reference = OperationReference {
                actor: run.worker_id.clone(),
                request_id: cause.clone(),
            };
            result["operationReference"] = json!(reference);
            let encoded = serde_json::to_string(&command)?;
            let fingerprint = format!("{:x}", Sha256::digest(encoded.as_bytes()));
            db.execute(
                "INSERT INTO requests(actor,id,fingerprint,command,result) VALUES(?1,?2,?3,?4,?5)",
                params![
                    run.worker_id,
                    cause,
                    fingerprint,
                    encoded,
                    serde_json::to_string(&result)?
                ],
            )?;
            let current: Task = load(db, "tasks", &task.id)?;
            let mut updated = run.clone();
            updated.task_revision = current.revision;
            crate::runs::save(db, &updated)?;
            return Ok(result);
        }
        MemberCommand::DecisionRead(input) => {
            let decision = decision_in_scope(db, run, task, &input.id)?;
            return Ok(json!(decision));
        }
        MemberCommand::DecisionRespond(input) => {
            let decision = decision_in_scope(db, run, task, &input.id)?;
            if run.purpose != "coordinate" || decision.handler != run.worker_id {
                return Err(Error::Forbidden(
                    "只能由本任务指定处理者正式回应决定请求".into(),
                ));
            }
            let result = apply(
                db,
                &run.worker_id,
                &operation.operation_id,
                &Command::DecisionRespond {
                    id: input.id.clone(),
                    revision: input.revision,
                    answer: input.answer,
                },
                workspace,
            )?;
            if decision.request_delivery == run.delivery_id {
                crate::disposition::record(
                    db,
                    run,
                    task,
                    json!({"kind":"decision_answer","decisionId":input.id,"decisionRevision":result["decision"]["revision"]}),
                )?;
            }
            return Ok(result);
        }
        MemberCommand::DecisionRecord(input) => {
            let decision = decision_in_scope(db, run, task, &input.id)?;
            if run.purpose != "coordinate" || decision.requester != run.worker_id {
                return Err(Error::Forbidden("只能由本任务原发起者落实正式回应".into()));
            }
            Command::DecisionRecord {
                id: input.id,
                revision: input.revision,
                resolution: input.resolution,
            }
        }
        MemberCommand::TaskIntake(input) => {
            authorize_intake(db, run, task)?;
            if matches!(input.decision, IntakeDecision::Accept)
                && task.contract.code_input.is_some()
                && !prepared.is_some_and(|p| {
                    Some(&p.input_id) == task.contract.code_input.as_ref()
                        && p.task_id == task.id
                        && p.revision == task.revision
                })
            {
                return Err(Error::Unavailable(
                    "代码承接须先在数据库线程外核对固定输入".into(),
                ));
            }
            let result = apply(
                db,
                &run.worker_id,
                &operation.operation_id,
                &Command::Intake {
                    id: task.id.clone(),
                    revision: input.revision,
                    decision: input.decision.clone(),
                    reason: input.reason.clone(),
                },
                workspace,
            )?;
            let current: Task = load(db, "tasks", &task.id)?;
            let mut updated = run.clone();
            updated.task_revision = current.revision;
            crate::runs::save(db, &updated)?;
            if !matches!(input.decision, IntakeDecision::Accept) {
                crate::disposition::record(
                    db,
                    &updated,
                    &current,
                    json!({"kind":"intake","decision":input.decision,"reason":input.reason}),
                )?;
            }
            return Ok(result);
        }
        MemberCommand::MessageRespond(disposition) => {
            return crate::disposition::save(db, run, task, &disposition);
        }
        MemberCommand::TaskRead(_) => {
            // No native conversation, credential, workspace path or other task.
            let verification_profile = task
                .contract
                .verification_profile
                .as_ref()
                .map(|id| {
                    let profile: crate::profile::ProfileRecord = load(db, "profiles", id)?;
                    Ok::<_, Error>(json!({"id":profile.id,"name":profile.specification.name,
                        "checkId":profile.specification.check_id}))
                })
                .transpose()?;
            let mut checks = Vec::new();
            let mut stmt = db.prepare("SELECT c.data FROM checks c JOIN runs r ON r.id=c.run_id WHERE r.task_id=?1 ORDER BY c.rowid")?;
            for row in stmt.query_map([&task.id], |r| r.get::<_, String>(0))? {
                let check: crate::verification::Check = serde_json::from_str(&row?)?;
                if check.task_id == task.id
                    && (task.team_snapshot.leader == run.worker_id || check.run_id == run.id)
                {
                    checks.push(
                        json!({"id":check.id,"runId":check.run_id,"target":check.target,
                        "state":check.state,"conclusion":check.conclusion}),
                    );
                }
            }
            let mut stmt =
                db.prepare("SELECT data FROM decisions WHERE task_id=?1 ORDER BY rowid")?;
            let rows = stmt
                .query_map([&task.id], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            let mut decisions = Vec::new();
            for row in rows {
                let d: DecisionRequest = serde_json::from_str(&row)?;
                if task.team_snapshot.leader == run.worker_id
                    || d.requester == run.worker_id
                    || d.handler == run.worker_id
                {
                    decisions.push(json!({"id":d.id,"kind":d.kind,"state":d.state,"revision":d.revision,"effectiveRevision":d.effective_revision}));
                }
            }
            return Ok(
                json!({"id":task.id,"goal":task.goal,"state":task.state,"revision":task.revision,
                "decisions":decisions,"deliveries":crate::retry::list(db,task,&run.worker_id)?,"blockers":crate::blocker::list(db,&task.id)?,"assignments":crate::assignment::list(db,&task.id)?,"reworks":crate::rework::list(db,&task.id)?,"currentArtifact":task.current_artifact,"handoffs":crate::handoff::list(db,&task.id)?,
                "contract":task.contract,"verificationProfile":verification_profile,"checks":checks,"deploy":task.deploy,"responsibilities":{"leader":task.team_snapshot.leader,"executor":task.team_snapshot.executor,"verifier":task.team_snapshot.verifier,"deployer":task.team_snapshot.deployer,"acceptor":task.team_snapshot.acceptor},
                "budget":{"runsUsed":task.runs_used,"messagesUsed":task.messages_used,"reworksUsed":task.reworks_used,"reworksReserved":crate::rework::reserved(db,&task.id)?}}),
            );
        }
        MemberCommand::MessageSend(input) => Command::MessageSend {
            task_id: task.id.clone(),
            recipient: input.recipient,
            kind: input.kind,
            body: input.body,
            reply_to: input.reply_to,
        },
        MemberCommand::DecisionRequest(input) => Command::DecisionRequest {
            task_id: task.id.clone(),
            revision: input.revision,
            handler: input.handler,
            question: input.question,
            impact: input.impact,
            options: input.options,
        },
    };
    apply(
        db,
        &run.worker_id,
        &operation.operation_id,
        &command,
        workspace,
    )
}

fn authorize_intake(db: &Connection, run: &Run, task: &Task) -> Result<()> {
    if run.purpose != "coordinate"
        || task.team_snapshot.leader != run.worker_id
        || !run.permissions.contains(&Permission::Arrange)
    {
        return Err(Error::Forbidden(
            "仅有获准安排权限的团队负责人可补齐、承接或安排任务".into(),
        ));
    }
    require(&task.team_snapshot, &run.worker_id, Permission::Arrange)?;
    require(
        &load::<Team>(db, "teams", &task.team_id)?,
        &run.worker_id,
        Permission::Arrange,
    )
}

fn decision_in_scope(db: &Connection, run: &Run, task: &Task, id: &str) -> Result<DecisionRequest> {
    let decision: DecisionRequest = load(db, "decisions", id)?;
    if decision.task_id != task.id
        || !(task.team_snapshot.leader == run.worker_id
            || decision.requester == run.worker_id
            || decision.handler == run.worker_id)
    {
        return Err(Error::Forbidden(
            "待决定事项不在当前任务和成员可见范围内".into(),
        ));
    }
    Ok(decision)
}
