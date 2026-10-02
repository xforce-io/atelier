//! Packaged guidance and core-selected host/member descriptions.
use crate::{
    Error, Result,
    model::*,
    store::{Store, load, require},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, io::Write, path::Path};
const ROOT: &str = include_str!("../skills/atelier/SKILL.md");
const COMMON: &str = include_str!("../skills/atelier/references/common.md");
const HOST: &str = include_str!("../skills/atelier/references/host.md");
const COORDINATION: &str = include_str!("../skills/atelier/references/coordination.md");
const EXECUTION: &str = include_str!("../skills/atelier/references/execution.md");
const VERIFICATION: &str = include_str!("../skills/atelier/references/verification.md");

/// Install only the host bootstrap. It contains no member tools or role choice.
pub fn install(destination: &Path) -> Result<Value> {
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent)?;
    let name = destination
        .file_name()
        .ok_or_else(|| Error::Invalid("指定 Skill 目录，不是根目录".into()))?;
    let path = parent.join(name);
    let entry = path.join("SKILL.md");
    let existing = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::Conflict(
                    "Skill 目标必须是独立目录，不跟随符号链接".into(),
                ));
            }
            let metadata = fs::symlink_metadata(&entry)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || fs::read(&entry)? != ROOT.as_bytes()
            {
                return Err(Error::Conflict(
                    "已有 Skill 内容不同，保留原文件；请显式选择新目录".into(),
                ));
            }
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(&path)?;
            let mut file = match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&entry)
            {
                Ok(file) => file,
                Err(error) => {
                    let _ = fs::remove_dir(&path);
                    return Err(error.into());
                }
            };
            let write = (|| -> Result<()> {
                file.write_all(ROOT.as_bytes())?;
                file.sync_all()?;
                fs::File::open(&path)?.sync_all()?;
                fs::File::open(&parent)?.sync_all()?;
                Ok(())
            })();
            if let Err(e) = write {
                // Only remove the files this invocation just created.
                let _ = fs::remove_file(&entry);
                let _ = fs::remove_dir(&path);
                return Err(e);
            }
            false
        }
        Err(e) => return Err(e.into()),
    };
    Ok(
        json!({"entry":entry,"unchanged":existing,"protocol":2,"digest":crate::content::digest(ROOT.as_bytes()),"next":"让宿主加载此 SKILL.md；文件安装成功不代表宿主已加载或团队已验收"}),
    )
}
fn operation(id: &str, command: &str, guidance: &str) -> Value {
    json!({"id":id,"command":command,"guidance":guidance})
}
pub fn describe(
    path: &Path,
    protocol: u8,
    team_id: Option<&str>,
    task_id: Option<&str>,
) -> Result<Value> {
    if protocol != 2 {
        return Err(Error::Invalid(
            "Skill 协议不兼容；本 CLI 只支持主版本 2".into(),
        ));
    }
    if team_id.is_some() && task_id.is_some() {
        return Err(Error::Invalid("team 与 task 互斥".into()));
    }
    let store = match Store::open(path) {
        Ok(store) => store,
        Err(Error::NotFound(_)) if team_id.is_none() && task_id.is_none() => {
            return Ok(
                json!({"protocol":2,"state":"workspace_missing","identity":null,"scope":{"workspace":path},"guidance":"先在用户指定的新目录或空目录执行 workspace init --name <本人名称>，再重新 describe；不自动覆盖旧内容。","operations":[operation("workspace.init","workspace init --name <本人名称>","创建工作区及唯一本人，不启动服务或任务。"),operation("doctor","doctor","查询缺失情况，不创建对象。")]}),
            );
        }
        Err(e) => return Err(e),
    };
    store.host_skill_description(team_id, task_id)
}
impl Store {
    pub fn host_skill_description(
        &self,
        team_id: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<Value> {
        if team_id.is_some() && task_id.is_some() {
            return Err(Error::Invalid("team 与 task 互斥".into()));
        }
        let tx = self.connection.unchecked_transaction()?;
        let actor: String = tx.query_row("SELECT self_id FROM workspace", [], |r| r.get(0))?;
        let actor = &actor;
        let task: Option<Task> = task_id.map(|id| load(&tx, "tasks", id)).transpose()?;
        let selected = task.as_ref().map(|t| t.team_id.as_str()).or(team_id);
        let team: Option<Team> = selected.map(|id| load(&tx, "teams", id)).transpose()?;
        if team.as_ref().is_some_and(|t| !t.members.contains(actor)) {
            return Err(Error::Forbidden("本人不在所选团队中".into()));
        }
        let allowed = |p: Permission| {
            team.as_ref()
                .is_some_and(|t| require(t, actor, p.clone()).is_ok())
                && task
                    .as_ref()
                    .is_none_or(|t| require(&t.team_snapshot, actor, p).is_ok())
        };
        let leader = task
            .as_ref()
            .map(|t| &t.team_snapshot.leader)
            .or(team.as_ref().map(|t| &t.leader))
            == Some(actor);
        let active = task
            .as_ref()
            .is_some_and(|t| t.state != "closed" && !t.cancellation_requested);
        let mut operations = vec![
            operation(
                "workspace.read",
                "workspace show; doctor",
                "查询本人、工作区、已支持接入和当前依赖；支持不代表就绪。",
            ),
            operation(
                "objects.read",
                "worker list/show; team list/show; task list/show; task decision list/show; task acceptance show; task blocker list/show; mailbox list; request show",
                "查询已保存事实；指定任务后重新 describe 获得其操作范围。",
            ),
            operation(
                "setup",
                "connection create/update/test; connection credential set/clear; worker create/update; team create",
                "本机管理入口；连接按 API/CLI 互斥配置。秘密只经 credential set --stdin；CLI 隔离接入须等实现并通过检查。用各命令 --help 取字段，写操作带 request-id。",
            ),
            operation(
                "runtime",
                "runtime start/status/stop/reconcile",
                "显式 start；stop 不取消任务；离线 reconcile 核对实际资源，不领取新消息。",
            ),
            operation(
                "inputs",
                "sample prepare; input import/list/show; profile import/list/show",
                "代码任务固定 Git commit 和可信检查镜像；不执行候选自带的任意宿主脚本。",
            ),
            operation(
                "export",
                "artifact show/export; run show; handoff show; verification show; check show",
                "读取证据；导出指定版本到新空目录，不改变验收。",
            ),
        ];
        if let Some(team) = &team {
            if require(team, actor, Permission::Manage).is_ok() {
                operations.push(operation(
                    "team.manage",
                    "team update; team permissions update",
                    "按当前团队 revision 显式维护职责及 grant/revoke；不会热更新冻结任务。",
                ));
            }
            if task.is_none() && allowed(Permission::Communicate) {
                operations.push(operation(
                    "task.create",
                    "task create --team <团队 ID> --goal <目标>",
                    "只保存 pending Task 及团队负责人投递；不自动承接。",
                ));
            }
        }
        if let Some(task) = &task {
            if active {
                let blockers: Vec<crate::blocker::Blocker> =
                    serde_json::from_value(crate::blocker::list(&tx, &task.id)?)?;
                if blockers.iter().any(|b| {
                    b.state == "open"
                        && crate::blocker::authorize_resolve(&tx, task, b, actor).is_ok()
                }) {
                    operations.push(operation("blocker.resolve","task blocker resolve <阻塞> --revision <版本> --task-revision <任务版本> --evidence <修复依据>","指定处理者或获准本人核对原范围修复依据；相关资源必须停止，解决不自动重新执行。"));
                }
            }
            if active && allowed(Permission::Manage) {
                operations.push(operation(
                    "task.cancel",
                    "task cancel <Task> --revision <版本> --reason <原因>",
                    "取消未开始投递并请求停止；未知资源未核对不得假报关闭。",
                ));
                if task.state == "pending" {
                    operations.push(operation("task.update","task update <Task> --revision <版本>","补齐 pending 契约，刷新职责/配置须显式 --refresh-team；已承接契约不能改写。"));
                }
            }
            if active && allowed(Permission::Communicate) {
                operations.push(operation("decisions","task decision list/show/request; message send; mailbox respond","发起补充事项或沟通；普通消息不替代正式决定或落实。发起请求带任务 revision；只处理本人收件箱。"));
                let mut stmt = tx.prepare("SELECT data FROM decisions WHERE task_id=?1")?;
                let rows = stmt
                    .query_map([&task.id], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let decisions = rows
                    .iter()
                    .map(|s| serde_json::from_str::<DecisionRequest>(s))
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                if decisions.iter().any(|d| {
                    d.handler == *actor
                        && d.kind != "acceptance"
                        && d.kind != "recovery"
                        && d.state == "open"
                }) {
                    operations.push(operation(
                        "decision.respond",
                        "task decision respond <事项> --revision <版本> --answer <答复>",
                        "只回应指定由本人处理的当前事项；正式答复不自动落实或扩权。",
                    ));
                }
                if decisions.iter().any(|d| {
                    d.requester == *actor
                        && d.kind != "acceptance"
                        && d.kind != "recovery"
                        && d.state == "responded"
                }) {
                    operations.push(operation("decision.record","task decision record <事项> --revision <版本> --operation-actor <actor> --operation-request <requestId>","原发起者记录已提交 operationReference；无需变化或受阻用互斥的 --no-change / --blocked 和原因，不能伪造成功。"));
                }
            }
            if active && leader && allowed(Permission::Arrange) && allowed(Permission::Communicate)
            {
                operations.push(operation("coordination","task intake; task execute/verify/rework; task acceptance request","本人是该任务冻结团队负责人才能承接/安排。查询当前版本；已受理执行不能原样重试；返工必需正式原因，验收请求必需当前产出及独立检验。"));
            }
            if active
                && task.team_snapshot.acceptor == *actor
                && allowed(Permission::Accept)
                && allowed(Permission::Communicate)
            {
                operations.push(operation("acceptance","task accept/reject <Task> --revision <版本> --request <事项 ID> --request-revision <版本> --reason <原因> --decision-ref <脱敏决定引用>","先展示当前请求及证据，只转交用户当前明确的接受/拒绝；没有决定保持待办，旧决定不能沿用于新版。核心仍核验检查和停止状态。"));
            }
            if active
                && allowed(Permission::Communicate)
                && ((task.team_snapshot.acceptor == *actor && allowed(Permission::Manage))
                    || (leader && allowed(Permission::Arrange)))
            {
                operations.push(operation(
                    "retry",
                    "mailbox retry <Delivery> --revision <版本> --reason <依据>",
                    "未知资源先核对；恢复事项须本人先选 retry；重试不重置预算或重新运行终局。",
                ));
            }
            if active
                && task.team_snapshot.acceptor == *actor
                && allowed(Permission::Manage)
                && allowed(Permission::Communicate)
            {
                operations.push(operation("recovery","task decision respond <事项> --revision <版本> --answer retry|wait|cancel; task recovery apply <事项> --revision <版本>","本人选择与落实分开；applied=false 时读取 blocked_reason，不视为恢复成功。回应不授权或解除 unknown。"));
            }
        }
        let permissions: Vec<_> = [
            Permission::Manage,
            Permission::Arrange,
            Permission::Execute,
            Permission::Verify,
            Permission::Handoff,
            Permission::Communicate,
            Permission::Accept,
        ]
        .into_iter()
        .filter(|p| allowed(p.clone()))
        .collect();
        let mut guidance = format!("{COMMON}\n{HOST}");
        if active && leader && allowed(Permission::Arrange) && allowed(Permission::Communicate) {
            guidance.push_str(COORDINATION);
        }
        let result = json!({"protocol":2,"state":if task.is_some(){"task"}else if team.is_some(){"team"}else{"workspace"},"identity":{"workerId":actor,"kind":"human"},"scope":{"workspace":self.workspace_path,"teamId":selected,"taskId":task_id,"taskRevision":task.as_ref().map(|t|t.revision),"authorizationRevision":team.as_ref().map(|t|t.authorization_revision)},"permissions":permissions,"isTeamLeader":leader,"guidance":guidance,"operations":operations,"notice":"索引表示身份与权限匹配，业务前置由实际命令再次核验；未选择团队不推断团队权限。"});
        tx.commit()?;
        Ok(result)
    }
}

/// The installed tool catalogue is the sole source for operation files. All
/// role-specific guidance is selected from those tools, never caller role text.
pub(crate) fn member_bundle(run: &Run, tools: &[Value]) -> Result<Value> {
    let mut files = BTreeMap::<String, String>::new();
    files.insert("references/common.md".into(), COMMON.into());
    let names = tools
        .iter()
        .map(|t| {
            t["name"]
                .as_str()
                .ok_or_else(|| Error::Invalid("工具登记缺少名称".into()))
        })
        .collect::<Result<Vec<_>>>()?;
    for (marker, path, body) in [
        ("task_arrange", "references/coordination.md", COORDINATION),
        ("artifact_submit", "references/execution.md", EXECUTION),
        (
            "verification_submit",
            "references/verification.md",
            VERIFICATION,
        ),
    ] {
        if names.contains(&marker) {
            files.insert(path.into(), body.into());
        }
    }
    let mut root = format!(
        "---\nname: atelier\ndescription: 处理当前绑定 Worker 的工作消息。\n---\n\n# Atelier 当前成员\n\n身份与 Task/Run/Delivery 由核心绑定，不能改用管理 CLI或自选角色。只用已装配工具，先 task_read，再按当前消息作决定。用途：{}。\n\n",
        run.purpose
    );
    for path in files.keys() {
        root.push_str(&format!("读取 [{path}]({path})。\n"));
    }
    for t in tools {
        let name = t["name"]
            .as_str()
            .ok_or_else(|| Error::Invalid("工具登记缺少名称".into()))?;
        if !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return Err(Error::Invalid("工具登记名称无效".into()));
        }
        let description = t["description"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Invalid("工具登记缺少说明".into()))?;
        if !t["inputSchema"].is_object() {
            return Err(Error::Invalid("工具登记缺少输入契约".into()));
        }
        let path = format!("operations/{name}.md");
        if files
            .insert(
                path.clone(),
                format!(
                    "# {name}\n\n{description}\n\n```json\n{}\n```\n",
                    serde_json::to_string_pretty(&t["inputSchema"])?
                ),
            )
            .is_some()
        {
            return Err(Error::Invalid("工具登记重复".into()));
        }
        root.push_str(&format!("- [{name}]({path})：{description}\n"));
    }
    let mut inline = root.clone();
    for (path, body) in &files {
        if path.starts_with("references/") {
            inline.push_str(&format!("\n{body}\n"));
        }
    }
    files.insert("SKILL.md".into(), root);
    Ok(
        json!({"skill":inline,"skillDigest":crate::content::digest(inline.as_bytes()),"files":files}),
    )
}
