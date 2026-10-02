//! Checks are core-observed evidence; a member's recommendation cannot manufacture it.
use crate::{
    Error, Result,
    artifact::Artifact,
    handoff::Handoff,
    member::{MemberBinding, bound_run},
    model::*,
    profile::ProfileRecord,
    store::{Message, Store, enqueue, load, new_id, text},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub isolation: Option<Value>,
    pub report: Option<Value>,
    pub id: String,
    pub run_id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub target: CheckTarget,
    pub content_digest: String,
    pub profile_id: String,
    pub container_name: String,
    pub state: String,
    pub conclusion: Option<String>,
    pub exit_code: Option<i64>,
    pub resources_stopped: bool,
    pub stdout: Option<LogRef>,
    pub stderr: Option<LogRef>,
    pub diagnostic: Option<String>,
    pub delivery_id: String,
    pub operation_id: String,
}
/// The target is fixed when the tool operation is recorded, before container startup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckTarget {
    Artifact { artifact_id: String },
    Candidate { run_id: String, revision: u64 },
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CheckInput {
    pub files: Vec<crate::content::FileEntry>,
    pub total_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRef {
    pub sha256: String,
    pub size: u64,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub id: String,
    pub task_id: String,
    pub task_revision: u64,
    pub artifact_id: String,
    pub worker_id: String,
    pub run_id: String,
    pub check_id: String,
    pub recommendation: String,
    pub reason: String,
    pub conclusion: String,
}
pub(crate) struct CheckPlan {
    pub workspace: PathBuf,
    pub epoch: String,
    pub check: Check,
    pub input: CheckInput,
    pub profile: ProfileRecord,
}
pub(crate) struct Observation {
    pub isolation: Option<Value>,
    pub exit_code: Option<i64>,
    pub resources_stopped: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub diagnostic: Option<String>,
}
pub(crate) struct SavedObservation {
    check: Check,
}
fn save(db: &Connection, c: &Check) -> Result<()> {
    db.execute("INSERT INTO checks(id,run_id,state,data) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET state=excluded.state,data=excluded.data",params![c.id,c.run_id,c.state,serde_json::to_string(c)?])?;
    Ok(())
}
fn accepted(db: &Connection, run: &Run, task: &Task) -> Result<Handoff> {
    if run.purpose != "verify"
        || !run.permissions.contains(&Permission::Verify)
        || task.team_snapshot.verifier.as_deref() != Some(run.worker_id.as_str())
    {
        return Err(Error::Forbidden(
            "仅冻结的独立检验成员可检查和提交检验建议".into(),
        ));
    }
    crate::handoff::claimable(db, task, &run.delivery_id)?;
    let data: String = db.query_row(
        "SELECT data FROM handoffs WHERE json_extract(data,'$.delivery_id')=?1",
        [&run.delivery_id],
        |r| r.get(0),
    )?;
    let h: Handoff = serde_json::from_str(&data)?;
    if h.state != "accepted" {
        return Err(Error::Conflict("接受交接前不能运行检查或提交检验".into()));
    }
    Ok(h)
}
fn self_test_allowed(db: &Connection, run: &Run, task: &Task) -> Result<()> {
    if !matches!(run.purpose.as_str(), "execute" | "rework")
        || task.state != "active"
        || !run.permissions.contains(&Permission::Execute)
        || task.team_snapshot.executor.as_deref() != Some(run.worker_id.as_str())
    {
        return Err(Error::Forbidden(
            "仅当前冻结执行成员可对本 Run 候选自测".into(),
        ));
    }
    if crate::artifact::has_submission(db, &run.id)?
        || crate::disposition::exists(db, &run.delivery_id)?
    {
        return Err(Error::Conflict(
            "本轮已提交产出或处理结果，不能启动自测".into(),
        ));
    }
    Ok(())
}
fn input(db: &Connection, check: &Check) -> Result<CheckInput> {
    match &check.target {
        CheckTarget::Artifact { artifact_id } => {
            let a: Artifact = load(db, "artifacts", artifact_id)?;
            if a.content_digest != check.content_digest || a.task_id != check.task_id {
                return Err(Error::Conflict("检查产出依据不匹配".into()));
            }
            Ok(CheckInput {
                files: a.files,
                total_bytes: a.total_bytes,
            })
        }
        CheckTarget::Candidate { run_id, .. } => {
            if run_id != &check.run_id {
                return Err(Error::Conflict("自测候选不属于本 Run".into()));
            }
            load(db, "check_inputs", &check.id)
        }
    }
}
pub(crate) fn request(
    db: &Connection,
    run: &Run,
    task: &Task,
    operation: &str,
    check_id: &str,
) -> Result<Value> {
    let (target, content_digest, snapshot) = if run.purpose == "verify" {
        let h = accepted(db, run, task)?;
        let artifact: Artifact = load(db, "artifacts", &h.artifact_id)?;
        (
            CheckTarget::Artifact {
                artifact_id: artifact.id,
            },
            artifact.content_digest,
            None,
        )
    } else {
        self_test_allowed(db, run, task)?;
        let candidate = crate::candidate::candidate(db, &run.id)?
            .ok_or_else(|| Error::Conflict("本 Run 候选尚未准备".into()))?;
        let total_bytes = crate::candidate::manifest(&candidate.files)?;
        let digest = crate::content::digest(&serde_json::to_vec(&candidate.files)?);
        (
            CheckTarget::Candidate {
                run_id: run.id.clone(),
                revision: candidate.revision,
            },
            digest,
            Some(CheckInput {
                files: candidate.files,
                total_bytes,
            }),
        )
    };
    let profile: ProfileRecord = load(
        db,
        "profiles",
        task.contract
            .verification_profile
            .as_deref()
            .ok_or_else(|| Error::Conflict("任务没有冻结检验配置".into()))?,
    )?;
    if profile.specification.check_id != check_id || profile.specification.record()? != profile {
        return Err(Error::Invalid("只能运行冻结配置中的命名检查".into()));
    }
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM checks WHERE run_id=?1 AND state!='finished')",
        [&run.id],
        |r| r.get(0),
    )?;
    if active {
        return Err(Error::Conflict(
            "本 Run 已有活动或未知检查，不能重复启动".into(),
        ));
    }
    let id = new_id();
    let c = Check {
        isolation: None,
        report: None,
        id: id.clone(),
        run_id: run.id.clone(),
        task_id: task.id.clone(),
        task_revision: task.revision,
        target,
        content_digest,
        profile_id: profile.id,
        container_name: format!("atelier-check-{id}"),
        state: "prepared".into(),
        conclusion: None,
        exit_code: None,
        resources_stopped: false,
        stdout: None,
        stderr: None,
        diagnostic: None,
        delivery_id: run.delivery_id.clone(),
        operation_id: operation.into(),
    };
    save(db, &c)?;
    if let Some(snapshot) = snapshot {
        db.execute(
            "INSERT INTO check_inputs(id,data) VALUES(?1,?2)",
            params![c.id, serde_json::to_string(&snapshot)?],
        )?;
    }
    Ok(json!({"check":c}))
}
pub(crate) fn read(db: &Connection, run: &Run, task: &Task, id: &str) -> Result<Check> {
    let c: Check = load(db, "checks", id)?;
    if c.task_id != task.id || (c.run_id != run.id && task.team_snapshot.leader != run.worker_id) {
        return Err(Error::Forbidden("检查证据不在当前任务的可见范围".into()));
    }
    Ok(c)
}
pub(crate) fn submit(
    db: &Connection,
    run: &Run,
    task: &Task,
    check_id: &str,
    recommendation: &str,
    reason: &str,
) -> Result<Value> {
    let h = accepted(db, run, task)?;
    text(reason, "检验建议依据", 65536)?;
    if !["pass", "fail", "inconclusive"].contains(&recommendation) {
        return Err(Error::Invalid(
            "检验建议只允许 pass/fail/inconclusive".into(),
        ));
    }
    let c = read(db, run, task, check_id)?;
    let latest: Option<String> = db
        .query_row(
            "SELECT id FROM checks WHERE run_id=?1 ORDER BY rowid DESC LIMIT 1",
            [&run.id],
            |r| r.get(0),
        )
        .optional()?;
    if c.run_id != run.id
        || c.state != "finished"
        || !c.resources_stopped
        || !matches!(&c.target, CheckTarget::Artifact { artifact_id } if artifact_id == &h.artifact_id)
        || c.task_revision != task.revision
        || Some(c.profile_id.as_str()) != task.contract.verification_profile.as_deref()
        || latest.as_deref() != Some(check_id)
    {
        return Err(Error::Conflict(
            "须引用本 Run 最新、已停止且匹配冻结产出的真实检查证据".into(),
        ));
    }
    let conclusion = match (c.conclusion.as_deref(), recommendation) {
        (Some("fail"), _) => "fail",
        (Some("pass"), "pass") => "pass",
        (Some("pass"), "fail") => "fail",
        _ => "inconclusive",
    };
    let v = Verification {
        id: new_id(),
        task_id: task.id.clone(),
        task_revision: task.revision,
        artifact_id: h.artifact_id,
        worker_id: run.worker_id.clone(),
        run_id: run.id.clone(),
        check_id: c.id,
        recommendation: recommendation.into(),
        reason: reason.into(),
        conclusion: conclusion.into(),
    };
    db.execute(
        "INSERT INTO verifications(id,run_id,task_id,artifact_id,data) VALUES(?1,?2,?3,?4,?5)",
        params![
            v.id,
            run.id,
            task.id,
            v.artifact_id,
            serde_json::to_string(&v)?
        ],
    )?;
    let mut updated = task.clone();
    let leader = task.team_snapshot.leader.clone();
    enqueue(db,&mut updated,&run.worker_id,&run.id,Message {recipient:&leader,kind:"result",body:&json!({"verificationId":v.id,"artifactId":v.artifact_id,"conclusion":v.conclusion,"reason":reason,"next":"由有权成员决定返工、等待或请求人工验收；本结果不关闭任务"}).to_string(),reply_to:None,event:Some(&format!("verification:{}:{leader}",run.id)),mandatory:true})?;
    crate::disposition::record(
        db,
        run,
        &updated,
        json!({"kind":"verification","verificationId":v.id}),
    )?;
    Ok(json!(v))
}
fn classify(profile: &ProfileRecord, o: &Observation) -> &'static str {
    if o.isolation.is_none()
        || !o.resources_stopped
        || o.diagnostic.is_some()
        || o.stdout_truncated
        || o.stderr_truncated
    {
        return "inconclusive";
    }
    let Ok(report) = serde_json::from_slice::<Value>(&o.stdout) else {
        return "inconclusive";
    };
    if report["checkId"] != profile.specification.check_id {
        return "inconclusive";
    }
    let Some(results) = report["results"].as_array() else {
        return "inconclusive";
    };
    if results.is_empty() || results.len() > 10000 {
        return "inconclusive";
    }
    let mut names = std::collections::BTreeSet::new();
    let mut failed = false;
    for item in results {
        let Some(name) = item["name"].as_str() else {
            return "inconclusive";
        };
        if name.is_empty() || !names.insert(name) {
            return "inconclusive";
        }
        match item["status"].as_str() {
            Some("pass") => {}
            Some("fail") => failed = true,
            _ => return "inconclusive",
        }
    }
    match (failed, o.exit_code) {
        (false, Some(0)) => "pass",
        (true, Some(1)) => "fail",
        _ => "inconclusive",
    }
}
impl CheckPlan {
    pub(crate) fn save_observation(self, observed: Observation) -> Result<SavedObservation> {
        let conclusion = classify(&self.profile, &observed).to_string();
        let log = |bytes: &[u8], truncated| -> Result<LogRef> {
            Ok(LogRef {
                sha256: crate::content::store_blob(&self.workspace, bytes)?,
                size: bytes.len() as u64,
                truncated,
            })
        };
        let mut c = self.check;
        c.isolation = observed.isolation;
        c.report = if observed.stdout.len() <= 65536 {
            serde_json::from_slice(&observed.stdout).ok()
        } else {
            None
        };
        c.stdout = Some(log(&observed.stdout, observed.stdout_truncated)?);
        c.stderr = Some(log(&observed.stderr, observed.stderr_truncated)?);
        c.resources_stopped = observed.resources_stopped;
        c.state = if c.resources_stopped {
            "finished"
        } else {
            "unknown"
        }
        .into();
        c.conclusion = Some(conclusion);
        c.exit_code = observed.exit_code;
        c.diagnostic = observed.diagnostic;
        Ok(SavedObservation { check: c })
    }
}
impl Store {
    /// Service startup only: recover resources from a previous epoch without
    /// granting a new member execution or releasing the owning Run.
    pub(crate) fn interrupted_checks(&self, epoch: &str) -> Result<Vec<CheckPlan>> {
        crate::runs::service(&self.connection, epoch, false)?;
        let mut query = self.connection.prepare(
            "SELECT c.data FROM checks c JOIN runs r ON r.id=c.run_id WHERE c.state IN ('running','unknown') AND json_extract(r.data,'$.epoch')!=?1",
        )?;
        let rows = query
            .query_map([epoch], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|data| {
                let check: Check = serde_json::from_str(&data)?;
                Ok(CheckPlan {
                    workspace: self.workspace_path.clone(),
                    epoch: epoch.into(),
                    input: input(&self.connection, &check)?,
                    profile: load(&self.connection, "profiles", &check.profile_id)?,
                    check,
                })
            })
            .collect()
    }
    pub fn check(&self, id: &str) -> Result<Check> {
        load(&self.connection, "checks", id)
    }
    pub fn verification(&self, id: &str) -> Result<Verification> {
        load(&self.connection, "verifications", id)
    }
    pub(crate) fn begin_check(
        &mut self,
        binding: &MemberBinding,
        id: &str,
    ) -> Result<Option<CheckPlan>> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (run, task) = bound_run(&tx, binding)?;
        let mut c = read(&tx, &run, &task, id)?;
        if c.run_id != run.id {
            return Err(Error::Forbidden("只能启动本 Run 的检查".into()));
        }
        if c.state != "prepared" {
            return Ok(None);
        }
        match &c.target {
            CheckTarget::Artifact { artifact_id } => {
                if accepted(&tx, &run, &task)?.artifact_id != *artifact_id {
                    return Err(Error::Conflict("检查与当前交接不匹配".into()));
                }
            }
            CheckTarget::Candidate { .. } => self_test_allowed(&tx, &run, &task)?,
        }
        let input = input(&tx, &c)?;
        let profile: ProfileRecord = load(&tx, "profiles", &c.profile_id)?;
        c.state = "running".into();
        save(&tx, &c)?;
        tx.execute(
            "UPDATE member_requests SET result=?3 WHERE delivery_id=?1 AND operation_id=?2",
            params![
                c.delivery_id,
                c.operation_id,
                serde_json::to_string(&json!({"ok":true,"data":{"check":c}}))?
            ],
        )?;
        tx.commit()?;
        Ok(Some(CheckPlan {
            workspace: self.workspace_path.clone(),
            epoch: run.epoch,
            check: c,
            input,
            profile,
        }))
    }
    pub(crate) fn finish_check(
        &mut self,
        epoch: &str,
        observed: SavedObservation,
    ) -> Result<Check> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::runs::service(&tx, epoch, false)?;
        let c = observed.check;
        let prior: Check = load(&tx, "checks", &c.id)?;
        if !["running", "unknown"].contains(&prior.state.as_str()) {
            return Err(Error::Conflict("检查不再属于当前完成操作".into()));
        }
        save(&tx, &c)?;
        tx.execute(
            "UPDATE member_requests SET result=?3 WHERE delivery_id=?1 AND operation_id=?2",
            params![
                c.delivery_id,
                c.operation_id,
                serde_json::to_string(&json!({"ok":true,"data":{"check":c}}))?
            ],
        )?;
        if !c.resources_stopped {
            let mut run: Run = load(&tx, "runs", &c.run_id)?;
            run.state = "unknown".into();
            run.stop_requested = true;
            run.stop_reason = Some("检查资源停止状态未知".into());
            crate::runs::save(&tx, &run)?;
            tx.execute("UPDATE deliveries SET status='uncertain',reason='检查资源状态未知',revision=revision+1 WHERE id=?1",[&run.delivery_id])?;
        }
        tx.commit()?;
        Ok(c)
    }
}
pub(crate) fn before_stop(db: &Connection, run: &Run) -> Result<()> {
    let active: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM checks WHERE run_id=?1 AND state IN ('running','unknown'))",
        [&run.id],
        |r| r.get(0),
    )?;
    if active {
        return Err(Error::Conflict("检查容器尚未核对停止，不能释放 Run".into()));
    }
    let mut s = db.prepare("SELECT data FROM checks WHERE run_id=?1 AND state='prepared'")?;
    let rows = s
        .query_map([&run.id], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for row in rows {
        let mut c: Check = serde_json::from_str(&row)?;
        c.state = "finished".into();
        c.resources_stopped = true;
        c.conclusion = Some("inconclusive".into());
        c.diagnostic = Some("Run 在检查启动前停止".into());
        save(db, &c)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> ProfileRecord {
        crate::profile::VerificationProfile {
            name: "test".into(),
            check_id: "rules".into(),
            image: format!("sha256:{}", "a".repeat(64)),
            argv: vec!["/check".into()],
        }
        .record()
        .unwrap()
    }
    fn observed(report: Value, code: i64) -> Observation {
        Observation {
            isolation: Some(json!({"fixture":true})),
            exit_code: Some(code),
            resources_stopped: true,
            stdout: serde_json::to_vec(&report).unwrap(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            diagnostic: None,
        }
    }
    #[test]
    fn report_requires_consistent_complete_evidence_not_just_zero_exit() {
        let p = profile();
        let valid = json!({"checkId":"rules","results":[{"name":"one","status":"pass"}]});
        assert_eq!(classify(&p, &observed(valid.clone(), 0)), "pass");
        assert_eq!(
            classify(
                &p,
                &observed(
                    json!({"checkId":"rules","results":[{"name":"one","status":"fail"}]}),
                    1
                )
            ),
            "fail"
        );
        for report in [
            json!({}),
            json!({"checkId":"other","results":[{"name":"one","status":"pass"}]}),
            json!({"checkId":"rules","results":[]}),
            json!({"checkId":"rules","results":[{"name":"one","status":"pass"},{"name":"one","status":"pass"}]}),
            json!({"checkId":"rules","results":[{"name":"one","status":"fail"}]}),
        ] {
            assert_eq!(classify(&p, &observed(report, 0)), "inconclusive");
        }
        assert_eq!(classify(&p, &observed(valid.clone(), 1)), "inconclusive");
        for flag in 0..5 {
            let mut o = observed(valid.clone(), 0);
            match flag {
                0 => o.stdout_truncated = true,
                1 => o.stderr_truncated = true,
                2 => o.resources_stopped = false,
                3 => o.isolation = None,
                _ => o.diagnostic = Some("timeout".into()),
            };
            assert_eq!(classify(&p, &o), "inconclusive");
        }
    }
}
