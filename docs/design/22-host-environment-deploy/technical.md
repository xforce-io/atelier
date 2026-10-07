# L2 技术设计：本机环境与由核心执行的部署

版本：v0.1，2026-10-07；状态：Draft。依据：[L1 v0.1](product.md)、[Issue #22](https://github.com/xforce-io/atelier/issues/22)。改写 [部署职责 L2 v0.1](../5-deploy-duty/technical.md) 第 4 节「提交」与「核心不派生部署进程」。工作区格式从 23 升到 24。

## 1. 数据

新表 `environments(id TEXT PRIMARY KEY, data TEXT NOT NULL)`，`data` 为 `Environment` JSON，并对 `$.name` 建唯一索引：

```rust
pub struct Environment {
    pub id: String,
    pub name: String,              // ^[a-z0-9][a-z0-9-]{0,62}$, immutable
    pub code_root: String,         // canonical absolute directory
    pub data_root: Option<String>, // canonical absolute directory
    pub service: Option<HostService>,
    pub revision: u64,
}
pub struct HostService {
    pub port: u16,                 // always bound to 127.0.0.1
    pub argv: Vec<String>,
    pub cwd: String,
    pub health_path: String,       // starts with '/'
}
```

已有 JSON 的扩展。所有新字段都是 `#[serde(default)]`，旧数据按「没有」读取：

- `Team.environment_grants: BTreeMap<String /* worker */, BTreeMap<String /* env name */, EnvAccess>>`，`EnvAccess` 取 `read` 或 `write`。
- `Contract.environments: Vec<String>`、`Contract.deploy_environment: Option<String>`。
- `Task.environment_snapshots: BTreeMap<String, Environment>`。
- `DeployRecord.state` 增加 `applying`；新增 `attempt: Option<DeployAttempt>`。

```rust
pub struct DeployAttempt {
    pub id: String,
    pub requested_by: String,
    pub run_id: Option<String>,    // None for the local human
    pub environment: Environment,  // frozen copy used for execution
    pub artifact_id: String,
    pub baseline_input_id: String,
    pub epoch: Option<String>,     // runtime that claimed it
    pub changes: Vec<DeployChange>,// path, action write|delete, before_sha, after_sha
    pub steps: Vec<DeployStep>,    // name, ok, detail, at
    pub backup_dir: Option<String>,
    pub log_path: Option<String>,
    pub old_pid: Option<u32>,
    pub new_pid: Option<u32>,
    pub restored: Option<bool>,
}
```

步骤名固定为 `precheck`、`backup`、`write`、`stop`、`start`、`health`、`restore`。

## 2. 工作区格式 23→24

`Store::open` 读到 23 时，在一个 `IMMEDIATE` 事务里建表、建索引，并把 `user_version` 设为 24。读到 24 时直接打开，其他版本仍按现有方式拒绝。`schema.sql` 改为 24，新建的工作区直接得到新表。`workspace` 查询返回 `schemaVersion: 24`。迁移只增加对象，不改写现有行。

## 3. 环境登记

新增 `Command::EnvironmentCreate`、`EnvironmentUpdate`，CLI 为 `atelier environment create|update|list|show`。启动命令写在 `--` 之后，每个参数一项：

```text
atelier environment create --name kairo-prod --code-root /Users/u/dev/github/kairo-prod \
  --port 8787 --cwd /Users/u/dev/github/kairo-prod --health-path / \
  -- /Users/u/dev/github/kairo-prod/.venv/bin/python -m kairo serve --port 8787
```

`update` 需要 `--revision`。可以替换任一字段，`--no-service` 清除服务，`--no-data-root` 清除数据目录，名称不可改。

权限：命令只在 CLI 管理入口受理，执行者必须是 `workspace.self_id`。成员工具不提供这两个命令。`dispatch` 拒绝来自成员绑定的同名调用。

校验在 `src/environment.rs::validate`，全部在事务内完成，第一处不合格即返回 `Error::Invalid`，指明字段：

1. 名称匹配正则，且未被占用。
2. 每个目录：
   - 是绝对路径，`fs::canonicalize` 后与输入完全相同；输入里含符号链接就会不同，因此被拒绝。
   - 是目录。
   - 不是 `/`，不等于 `$HOME`，也不是 `$HOME` 的上级。
   - 与 `canonicalize(workspace)` 互不包含。
   - 不包含 `$HOME/.ssh`、`$HOME/.gnupg`、`$HOME/Library/Keychains`、`$HOME/.config/gh`，也不在它们之内。
3. 本环境的 `code_root`、`data_root` 与其他环境的任一目录互不包含。同一环境内允许 `data_root` 位于 `code_root` 之下。
4. 服务：
   - 端口非 0，且未被其他环境登记。
   - `argv` 非空，最多 64 项。`argv[0]` 是绝对路径，指向存在的普通可执行文件。
   - 每项非空，不含空白或控制字符，最长 4096 字节。
   - `cwd` 满足第 2 条中前三项的路径规则。
   - `health_path` 以 `/` 开头，最长 1024 字节，只含可见 ASCII。

环境不缓存在运行服务内存里。每次使用都从库里读取，因此新增后不需要重启。

## 4. 团队授权

`team create|update` 增加两个参数：

- `--environment-grant WORKER:ENV:read|write`，可重复。
- `--environment-revoke WORKER:ENV`，可重复。

`validate_team` 增加以下检查：

- 环境存在。
- Worker 是团队成员。
- `write` 只能授给 `deployer`。
- 更换 `deployer` 时，旧成员的 `write` 必须同时撤销，否则拒绝。

授权变化计入 `authorization_revision`。

## 5. 任务契约与冻结

`task create`、`task update` 增加两个参数：

- `--environment NAME`，可重复。
- `--deploy-environment NAME`，自动并入 `environments`。

`validate_contract_refs` 检查：

- 名称存在。
- 有 `deploy_environment` 时，必须有 `code_input`。

冻结时机与 `team_snapshot` 相同：

- 创建时，或 `task update` 带 `refresh_team` 时，把点名的环境写入 `environment_snapshots`。
- 只补环境而不刷新团队时，也重新冻结环境，并且只冻结这次点名的环境。

## 6. 接受

`deploy::block_reason` 在现有检查之后，按顺序增加以下判断。任一不满足，就返回中文原因：

1. `contract.deploy_environment` 存在，否则原因是「任务没有点名部署目标环境」。
2. 当前和冻结的 `environment_grants[deployer][env]` 都是 `write`。
3. 当前环境存在，且除 `revision` 以外的字段与冻结副本相等。

部署投递正文的 `instruction` 改为：「使用 deploy_apply 发起部署；结果由核心执行后记录，不能自报。」

## 7. 发起部署

成员工具 `deploy_apply { revision }` 只在 `purpose=deploy` 的运行中提供，替换 `task_deploy`。CLI 为 `task deploy apply --id T --revision N`，取消 `task deploy --result`。两者都进入 `deploy::apply`，在同一事务内：

1. 复用 `report` 现有的身份与运行核对：冻结部署成员；数字员工须在自己唯一的运行中的部署 Run 内；本人须没有活动 Run。还要核对当前与冻结的 `task.deploy`。
2. 任务为 `active`，`deploy.state=open`，`revision` 匹配。
3. 重复第 6 节的三项判断，任一不满足即返回 `Conflict`，部署记录不变。
4. 运行服务存在且未请求停止，否则返回 `Unavailable`，提示「运行服务未运行，不能执行部署」。
5. 不存在另一项 `deploy.state=applying` 且 `attempt.environment.name` 相同的任务。
6. 写入 `attempt`：`environment` 取冻结副本，`artifact_id` 取验收记录对应的产出，`baseline_input_id` 取 `contract.code_input`，`epoch` 为空。然后 `state=applying`，`revision += 1`。

成员调用沿用 `member_requests` 账本，同一 `operationId` 返回同一结果。返回 `{attemptId}`，不等待执行完成。部署运行随后照常给出处理结果，引用这次请求。

## 8. 执行

### 8.1 领取

运行服务主循环每轮调用 `deploy::claim(epoch)`，把 `applying` 且 `epoch` 为空的尝试领为本 epoch。领取后在新的异步任务里执行 `host_deploy::execute(plan)`，不占用数据库线程，方式与 `checker::execute` 相同。每完成一步，就用一次短事务追加 `steps` 和其余字段，以便中断后核对进度。

### 8.2 步骤

1. **precheck**：
   - 用 `export` 现有的校验，核对产出清单和每个 blob。
   - 以 `(sha256, size, executable)` 比较产出与基线，得到改动集合。只在基线里的路径为 `delete`，其余为 `write`。改动集合为空则失败，原因是「产出与基线没有差异」。
   - 以下路径一律拒绝：首个路径分量为 `.git`；连接 `code_root` 后落在 `data_root` 之内。
   - 生产文件的核对，逐个路径分量用 `symlink_metadata` 检查，遇到符号链接即失败：
     - 基线中有该文件：生产文件必须是普通文件，摘要与可执行位都与基线相同。
     - 基线中没有该文件：生产中必须不存在。
   - 有服务时，识别监听进程：
     - 执行 `/usr/sbin/lsof -nP -iTCP@127.0.0.1:{port} -sTCP:LISTEN -Fp`。多于一个进程号就失败。
     - 用 `ps -ww -o uid=,args= -p {pid}` 核对：uid 等于当前用户，`args` 等于 `argv.join(" ")`。
     - 用 `lsof -a -p {pid} -d cwd -Fn` 核对工作目录等于登记的 `cwd`。
     - 没有监听进程时，记录 `old_pid=None`，继续执行。
2. **backup**：
   - 在 `{workspace}/deploy-backups/{attemptId}/` 下保存所有 `write` 与 `delete` 路径的原文件，以及 `manifest.json`。`manifest.json` 记录路径、原摘要、可执行位和原文件是否存在。
   - 写完后 `sync_all`。备份目录已经存在就失败。
3. **write**：
   - `write`：先在同一目录写临时文件 `.atelier-deploy-{attemptId}`，按需设置 `0o755` 或 `0o644`，`sync_all` 后再 `rename`。缺少的父目录逐级创建，每一级都要求位于 `code_root` 内，且不是符号链接。
   - `delete`：删除该文件，不删除空目录。
   - 每个文件写完后，在 `changes` 里记下 `after_sha`。
4. **stop**：有 `old_pid` 时，执行 `/bin/kill -TERM {pid}`。每 250ms 检查一次，15 秒内要求该进程号不存在，且端口上没有监听进程。超时即失败，不发送 `SIGKILL`。
5. **start**：
   - 用 `std::process::Command` 启动 `argv`：工作目录为 `cwd`，`process_group(0)`，标准输入为空，标准输出和标准错误追加到 `{workspace}/deploy-logs/{attemptId}.log`。
   - 环境变量继承运行服务的环境，不增加任何变量。
   - 启动后把子进程句柄交给后台任务等待退出，防止僵尸进程。运行服务退出后，该进程由系统接管，不随运行服务结束。
6. **health**：
   - 每 500ms 用 `tokio::net::TcpStream` 连接 `127.0.0.1:{port}`，发送 `GET {health_path} HTTP/1.1`，带 `Host: 127.0.0.1` 和 `Connection: close`，只读取状态行。
   - 状态码为 200，且 `lsof` 看到的监听进程号等于 `new_pid` 时通过。
   - 30 秒内未通过即失败。新进程提前退出也立即失败。

没有登记服务时，只执行第 1 至 3 步。

### 8.3 失败与恢复

- **precheck 或 backup 失败**：不执行恢复，`restored=None`，生产未被改动。
- **write、stop、start 或 health 失败**，执行 `restore`：
  1. 按 `manifest.json` 写回原文件，删除新增的文件。
  2. `new_pid` 若仍存活，按第 4 步的方式停止它。
  3. 原服务若已停止，按第 5、6 步用登记的命令重新启动，并做健康检查。原服务仍在运行时，比如 stop 超时，就不重启。
  4. 全部成功则 `restored=true`，任一失败则 `restored=false`。

### 8.4 结束

结束的事务复用 `deploy::report` 中关闭与失败的分支，只在核心内部调用：

- **成功**：`state=succeeded`，任务关闭，`outcome=deployed`。按现有方式取消剩余投递，结束尚未领取的 `assignment.deploy`。
- **失败**：`state=failed`。`reason` 写明失败步骤，以及 `restored` 的结果。向团队负责人投递 `failure`，正文包含 `attemptId`、失败步骤、`restored` 和备份目录。

结束事务要求 `epoch` 仍是本运行服务，否则放弃写入。

### 8.5 中断

`runtime::serve` 启动时、`reconcile` 时，都在 `runtime_recover_checks` 之后调用 `deploy::recover(epoch)`。凡是 `applying` 且 `epoch` 为其他值的尝试，一律标为 `failed`，原因是「部署中断」，列出已完成的步骤和备份目录，并投递失败通知。这一步不做文件或进程操作，由本人按记录核对生产。

## 9. 只读查看

`list_files`、`read_file` 增加可选参数 `environment` 和 `root`。`root` 取 `code` 或 `data`，缺省为 `code`。

提供条件：本 Run 的 Worker 在当前和冻结的 `environment_grants` 中对任务点名的某个环境有授权。这两个工具已经提供时只增加参数；否则只提供这两个工具，并要求必须带 `environment`。

实现放在 `src/environment.rs::read`，在宿主上直接读取：

- 相对路径校验与 `candidate` 相同。
- 每个路径分量用 `symlink_metadata` 检查，符号链接一律拒绝。
- 分页与大小限制与候选文件相同：每页 20 项，每次最多 32 KiB。

`write_file`、`delete_file` 不接受 `environment`。不新增容器挂载；生产端口不进入成员网络。

## 10. Skill

- `host.md` 增加一段：用户要求「管起来」某个项目时，问齐名称、代码目录、可选数据目录和服务字段，再调用 `environment create`，不猜测端口或命令。环境不登记凭据。
- `SKILL.md` 的部署说明改为：只用 `deploy_apply`，结果以 `task show` 中的部署记录为准，不自报。
- 成员工具描述同步更改。

## 11. 测试映射

| L1.8 | 检查 |
|---|---|
| S1.A1 | `environment_create_update_and_show_without_runtime_restart`（core）；CLI 测试 `environment_cli_round_trip` |
| S1.A2 | `environment_registration_rejects_unsafe_paths_commands_and_overlaps`，每类输入一个用例 |
| S1.A3 | verify feature `host-environment-deploy.md` 的宿主路径，不由单元测试代替 |
| S1.A4 | `member_tools_do_not_offer_environment_registration` |
| S2.A1 | `environment_write_grant_is_only_for_the_deployer` |
| S2.A2 | `task_freezes_environment_and_grants` |
| S2.A3 | `accept_without_deploy_environment_or_write_grant_blocks_deploy` |
| S2.A4 | 既有接受关闭测试保持有效 |
| S2.A5 | `contract_rejects_unknown_environment_and_deploy_without_code_input` |
| S3.A1 | `host_deploy_replaces_changed_files_and_restarts_service`：临时目录、`python3 -m http.server` 风格的夹具服务、真实运行服务 |
| S3.A2 | `host_deploy_refuses_when_production_drifted_from_baseline` |
| S3.A3 | `host_deploy_refuses_unregistered_listener`、`host_deploy_starts_when_nothing_listens` |
| S3.A4 | `host_deploy_restores_files_and_service_after_failed_health_check` |
| S3.A5 | `deploy_apply_rejects_non_deployer_outside_run_and_after_end`；`task_deploy` 已不在工具清单 |
| S3.A6 | CLI 测试 `human_deploy_apply_requires_running_runtime` |
| S3.A7 | `interrupted_deploy_is_recorded_failed_on_recovery` |
| S4.A1 | `environment_read_lists_and_reads_granted_files` |
| S4.A2 | `environment_read_rejects_ungranted_traversal_symlink_and_writes` |

原有的 `deploy_success_closes_separately_from_acceptance`、`human_deploy_success_handles_the_unclaimed_assignment`、`deploy_failure_keeps_the_task_unfinished`、`human_deploy_failure_handles_the_unclaimed_assignment` 改为经 `deploy_apply` 驱动，断言保持：成功与验收分开记录，失败时任务不关闭，尚未领取的投递被结束。

夹具服务的健康路径可以通过文件开关返回 500，用来驱动 S3.A4。所有测试只使用临时目录和临时端口。

## 12. 风险与兼容

- **工作区升级不可回退。** 升级到 24 后，旧版本二进制打不开该工作区。这是首次迁移，只增加对象。
- **只支持 macOS。** 识别监听进程依赖 `/usr/sbin/lsof` 与 `ps` 的 macOS 行为。Linux 主机需要另行验证，本版遇到时直接失败，不做替代实现。
- **逐字比对命令行。** 以 `ps args` 和 `argv.join(" ")` 比较，是因为不允许参数含空白；用 `lsof` 和 `ps` 而不引入 `libc`，以保持 safe Rust。
- **环境变量。** 新进程继承运行服务的环境变量。运行服务的环境若与原服务不同，新服务可能起不来，此时健康检查失败并恢复。这一点见 L1 第 9 节第 1 个待定问题。
- **不支持代码目录以外的改动。** 产出里的路径都相对于 `code_root`，本版不支持部署到其他目录。
