# L2 技术设计：部署成员在本机部署，由核心核对结果

版本：v0.4，2026-10-07；状态：Draft。依据：[L1 v0.4](product.md)、[Issue #22](https://github.com/xforce-io/atelier/issues/22)。改写 [部署职责 L2 v0.1](../5-deploy-duty/technical.md) 第 4 节「提交」。工作区格式从 23 升到 24。

## 1. 数据

新表 `environments(id TEXT PRIMARY KEY, data TEXT NOT NULL)`，并对 `json_extract(data,'$.name')` 建唯一索引：

```rust
pub struct Environment {
    pub id: String,
    pub name: String,                  // ^[a-z0-9][a-z0-9-]{0,62}$, immutable
    pub code_root: String,             // canonical absolute directory
    pub service: Option<HostService>,
    pub verification: VerifyMethod,    // files (default) | command
    pub approval: CommandApproval,     // ask (default) | auto
    pub revision: u64,
}
pub enum VerifyMethod {
    Files,
    Command { argv: Vec<String>, timeout_seconds: u32 }, // runs in code_root
}
pub struct HostService {
    pub port: u16,                     // probed on 127.0.0.1 only
    pub health_path: String,           // starts with '/'
}
```

已有 JSON 的扩展。新字段都是 `#[serde(default)]`，旧数据按「没有」读取：

- `Contract.deploy_environment: Option<String>`
- `Task.environment_snapshot: Option<Environment>`
- `DeployRecord` 增加以下字段：

```rust
pub export: Option<DeployExport>,          // set when state becomes open
pub commands: Vec<HostCommand>,
pub verifications: Vec<DeployVerification>,

pub struct DeployExport {
    pub dir: String,                       // read-only copy of the accepted artifact
    pub artifact_id: String,
    pub baseline_input_id: String,
    pub changes: Vec<DeployChange>,        // path, action write|delete, sha256, executable
}
pub struct HostCommand {
    pub id: String,
    pub run_id: String,
    pub argv: Vec<String>,
    pub cwd: String,                       // relative to code_root
    pub timeout_seconds: u32,
    pub reason: String,
    pub state: String,                     // awaiting_approval|queued|running|exited|timed_out|rejected|cancelled|interrupted
    pub decision_id: Option<String>,
    pub epoch: Option<String>,
    pub pgid: Option<u32>,
    pub exit_code: Option<i32>,
    pub output_tail: Option<String>,       // last 16 KiB, lossy UTF-8
    pub log_path: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
}
pub struct DeployVerification {
    pub id: String,
    pub requested_by: String,
    pub state: String,                     // queued|running|passed|failed|interrupted
    pub epoch: Option<String>,
    pub method: String,                    // files | command
    pub mismatches: Vec<String>,           // files: "path: reason"
    pub exit_code: Option<i32>,            // command
    pub output_tail: Option<String>,       // command, last 16 KiB
    pub log_path: Option<String>,          // command
    pub health: Option<String>,
    pub ended_at: Option<String>,
}
```

`DeployRecord.state` 仍是 `open`、`blocked`、`succeeded`、`failed`。本设计不再产生 `failed`，旧记录照常读取。

## 2. 工作区格式 23→24

`Store::open` 读到 23 时，在一个 `IMMEDIATE` 事务里建表、建索引，并把 `user_version` 设为 24。读到 24 时直接打开，其他版本仍按现有方式拒绝。`schema.sql` 改为 24，新建工作区直接得到新表。`workspace` 查询返回 `schemaVersion: 24`。迁移只增加对象，不改写现有行。

## 3. 环境登记

新增 `Command::EnvironmentCreate`、`EnvironmentUpdate`，CLI 为 `atelier environment create|update|list|show`：

```text
atelier environment create --name kairo-prod --code-root /Users/u/dev/github/kairo-prod \
  --port 8787 --health-path / [--approval auto] \
  [--verify-timeout 120 -- /usr/bin/env bash -lc 'test "$(cat VERSION)" = "$ATELIER_ARTIFACT_DIGEST"']
```

写了 `--` 之后的参数，核对方式就是 `command`，否则是 `files`。`update` 需要 `--revision`，可以替换任一字段；`--no-service` 清除服务，`--verify-files` 改回文件比对，名称不可改。`--approval auto` 的输出里带一段固定的英文警告。

权限：只在 CLI 管理入口受理，执行者必须是 `workspace.self_id`。成员工具不提供这两个命令。

校验放在 `src/environment.rs::validate`，第一处不合格即返回 `Error::Invalid`，指明字段：

1. 名称匹配正则，且未被占用。
2. `code_root` 的要求：
   - 是绝对路径，`fs::canonicalize` 后与输入完全相同，因此含符号链接的路径会被拒绝。
   - 是目录。
   - 不是 `/`，不等于 `$HOME`，也不是 `$HOME` 的上级。
   - 与 `canonicalize(workspace)` 互不包含。
   - 不包含 `$HOME/.ssh`、`$HOME/.gnupg`、`$HOME/Library/Keychains`、`$HOME/.config/gh`，也不在它们之内。
3. 与其他环境的 `code_root` 互不包含。
4. `port` 非 0，且未被其他环境登记；`health_path` 以 `/` 开头，最长 1024 字节，只含可见 ASCII。
5. 核对命令：
   - `argv` 有 1 至 64 项，`argv[0]` 为绝对路径，每项不含 NUL，最长 64 KiB。
   - `timeout_seconds` 在 1 至 600 之间，缺省 120。

环境不缓存在运行服务内存里，每次使用都从库里读取。

## 4. 任务契约与冻结

`task create`、`task update` 增加 `--deploy-environment NAME`。`validate_contract_refs` 要求：

- 名称存在。
- 有部署目标时也有 `code_input`。

冻结时机与 `team_snapshot` 相同：创建时，或 `task update` 改变了 `deploy_environment` 或带 `refresh_team` 时，写入 `environment_snapshot`。

「一致」的定义：当前 `Environment` 存在，且除 `revision` 以外的字段与快照逐一相等。

## 5. 接受与导出

`deploy::block_reason` 在现有检查之后，依次增加两项：

1. 有 `contract.deploy_environment` 与 `environment_snapshot`，否则原因是「任务没有点名部署目标环境」。
2. 当前登记与快照一致，否则原因是「部署目标登记已变化」。

不阻塞时，`open_after_acceptance` 在同一事务内完成：

1. 加载验收记录对应的产出和 `code_input` 基线，按 `(sha256, size, executable)` 比较，得到 `changes`。
   - 只在基线中出现的路径记为 `delete`，其余有差异的路径记为 `write`。
   - `changes` 为空则阻塞，原因是「产出与基线没有差异」。
2. 导出产出：
   - 复用 `export_artifact` 的清单与 blob 校验，写入 `{workspace}/deploy-exports/.tmp-{uuid}`，再改名为 `{workspace}/deploy-exports/{acceptanceId}`。
   - 改动清单另写为 `{workspace}/deploy-exports/{acceptanceId}.changes.json`。
   - 文件权限设为 `0o444` 或 `0o555`，目录设为 `0o555`。
   - 目标目录已存在即失败。
   - `export_artifact` 对工作区内部的禁止规则不放宽；这里使用新的内部函数，目标目录固定，不接受调用方传入。
3. 写入 `export`，`state=open`，投递 `assignment.deploy`。正文包括导出目录、改动清单，以及固定说明：「用 host_exec 完成部署，做完用 deploy_verify 请核心核对；不能自报结果。」

导出失败返回 `Error::Io`，整个接受事务回滚。中途退出留下的 `.tmp-*` 目录不会被引用。

## 6. 本机命令

### 6.1 提交

成员工具 `host_exec` 只在 `purpose=deploy` 的运行中提供：

```json
{"argv": ["..."], "cwd": ".", "timeoutSeconds": 600, "reason": "..."}
```

`host_command::submit` 在成员调用事务内检查以下各项：

- 冻结部署成员。
- 当前与冻结的 `task.deploy`。
- 运行为本人的部署 Run。
- `deploy.state=open`。
- 当前登记与快照一致。
- `argv` 有 1 至 64 项；`argv[0]` 为绝对路径；每项不含 NUL，最长 64 KiB。
- `cwd` 是安全相对路径，连接 `code_root` 后 `canonicalize`，结果仍在 `code_root` 内。
- `timeoutSeconds` 在 1 至 3600 之间。
- `reason` 非空。
- 没有 `awaiting_approval`、`queued` 或 `running` 的命令。

确认方式为 `ask` 时，新建 `DecisionRequest`：

- `kind=host_command`，处理者为 `self_id`，选项 `["执行","拒绝"]`。
- `question` 包含环境名、绝对工作目录、`argv` 的 JSON 和理由。
- `impact` 为固定说明：「以本机用户身份执行，不做沙箱；可访问该用户能访问的一切，包括 Atelier CLI 与工作区。」

命令状态为 `awaiting_approval`。确认方式为 `auto` 时，命令直接为 `queued`。

工具返回 `{commandId, state}`，不等待执行。

### 6.2 确认

`DecisionRespond` 遇到 `kind=host_command` 时，改由 `host_command::respond` 处理，与 `recovery` 的分派方式相同。

- 只接受处理者本人的回答。
- 「执行」：命令改为 `queued`。
- 「拒绝」：命令改为 `rejected`，并向部署成员投递 `host_command.result`。

部署结束或任务取消时，`decisions::supersede` 作废仍在等待的事项，命令改为 `cancelled`。

### 6.3 执行

运行服务主循环每轮调用 `host_command::claim(epoch)`，把 `queued` 领为本 epoch 并改为 `running`，然后在独立的异步任务里执行，不占用数据库线程：

- 用 `tokio::process::Command` 启动 `argv[0]`，参数为其余各项，工作目录为 `code_root/cwd`。
  - `process_group(0)`，标准输入为空。
  - 标准输出与标准错误追加到 `{workspace}/host-commands/{id}.log`。
  - 环境变量继承运行服务。
- 启动后，立即把 `pgid` 写入记录。
- 超时处理：
  1. 执行 `/bin/kill -TERM -{pgid}`。
  2. 5 秒后仍有进程，就执行 `/bin/kill -KILL -{pgid}`。
  3. 状态记为 `timed_out`。
- 正常退出时状态为 `exited`，记录 `exit_code`。
- 进程组里残留的后台子进程按超时的方式结束，进程组即以 `-{pgid}` 发送信号的那一组。
- 结束事务写入 `output_tail`、`ended_at`，向部署成员投递 `host_command.result`，正文包含 `commandId`、状态与退出码。
- 结束事务要求 `epoch` 仍是本运行服务。

### 6.4 中断

`runtime::serve` 启动时、`reconcile` 时，都在 `runtime_recover_checks` 之后执行：

- `running` 且 `epoch` 不同的命令记为 `interrupted`，保留 `pgid`，并投递结果。
- `queued` 且 `epoch` 不同的命令退回为未领取。这类命令尚未启动，可以执行。

## 7. 核对

成员工具 `deploy_verify { revision }` 只在部署运行中提供。CLI 为 `task deploy verify --id T --revision N`；本人须没有活动 Run，且运行服务在运行。

`deploy::verify_request` 检查以下各项：

- 身份与授权，同 6.1。
- `state=open`。
- 当前登记与快照一致。
- 没有进行中的命令或核对。
- 运行服务在运行，否则返回 `Unavailable`。

检查通过后，追加一条 `queued` 核对，返回 `{verificationId}`。CLI 随后每 500ms 轮询一次，最多 60 秒，直到核对结束，并输出结果。

运行服务领取后，在独立任务里执行：

1. 按快照的 `verification` 执行：
   - `files`：对每个 `changes` 项，从 `code_root` 起逐个路径分量用 `symlink_metadata` 检查，遇到符号链接即记为不一致。
     - `write`：必须是普通文件，摘要与可执行位都与清单相同。
     - `delete`：必须不存在。
   - `command`：执行方式与 6.3 相同，只有以下几处不同：
     - 工作目录为 `code_root`。
     - 超时取登记值。
     - 日志写入 `{workspace}/deploy-verifications/{id}.log`。
     - 不经确认。
     - 在继承的环境变量之外，增加以下变量：
       - `ATELIER_CODE_ROOT`
       - `ATELIER_EXPORT_DIR`
       - `ATELIER_CHANGES_FILE`：改动清单的 JSON 文件，位于导出目录旁，只读。
       - `ATELIER_ARTIFACT_ID`
       - `ATELIER_ARTIFACT_DIGEST`：产出的 `content_digest`。
       - `ATELIER_TASK_ID`
     - 退出码为 0 即通过。超时或非 0 即不通过，并记录 `exit_code` 与 `output_tail`。
2. 有服务时做健康检查：
   - 每 500ms 用 `tokio::net::TcpStream` 连接 `127.0.0.1:{port}`，发送 `GET {health_path} HTTP/1.1`，带 `Host: 127.0.0.1` 和 `Connection: close`，只读取状态行。
   - 30 秒内收到 200 即通过。
3. 结束事务按结果处理：
   - 全部通过时为 `passed`：复用 `deploy::report` 中成功关闭的分支，改为内部调用。效果是关闭任务、`outcome=deployed`、作废待决定事项与阻塞、取消剩余投递、结束尚未领取的 `assignment.deploy`。
   - 否则为 `failed`：写入 `mismatches` 与 `health`，部署保持 `open`，并向部署成员投递 `deploy.verification`。

中断恢复时，`running` 的核对记为 `interrupted`，不推定结果。

## 8. 取消与保留的入口

- 取消：
  - 成员工具 `task_deploy`。
  - `Command::DeployReport`。
  - CLI `task deploy --result`。
  - `DeployResult`，若不再有使用者也一并删除。
- 保留：`task_report_blocker`。部署运行可以用它把阻塞交给团队负责人或本人，部署记录不变。
- `participants` 规则保持不变：部署记录为 `open` 时，部署成员是参与者。

## 9. Skill

- `host.md`：
  - 登记前问齐名称、代码目录、端口与健康检查路径，不猜测。
  - 问清部署方式。若服务不是直接从代码目录运行，比如镜像、编译产物、构建产物或已安装的包，就建议一条核对命令，经用户确认后再登记。
  - 用户要求自动执行时，先说明其含义。
  - `pendingDecisions` 里 `host_command` 一类事项，要原样展示参数、工作目录、理由与风险说明，用户明确同意后才回答「执行」。
- `SKILL.md` 的部署说明：从导出目录取文件，用 `host_exec` 完成改动，用 `deploy_verify` 请求核对，做不下去就报告阻塞。
- 成员工具描述同步更改。

## 10. 测试映射

| L1.8 | 检查 |
|---|---|
| S1.A1 | `environment_create_update_and_show_without_runtime_restart`；CLI `environment_cli_round_trip` |
| S1.A2 | `environment_registration_rejects_unsafe_paths_ports_and_overlaps`，每类输入一个用例 |
| S1.A3 | verify feature `host-environment-deploy.md` 的宿主路径 |
| S1.A4 | `member_tools_do_not_offer_environment_registration` |
| S2.A1 | `accept_exports_read_only_artifact_and_change_list`、`task_keeps_frozen_environment_after_registration_change` |
| S2.A2 | `accept_without_deploy_environment_or_with_changed_registration_blocks` |
| S2.A3 | 既有接受关闭测试保持有效 |
| S2.A4 | `contract_rejects_unknown_environment_and_deploy_without_code_input` |
| S3.A1 | `host_command_waits_for_approval_then_runs`：临时目录、真实运行服务 |
| S3.A2 | `rejected_host_command_never_runs` |
| S3.A3 | `auto_approval_runs_without_decision` |
| S3.A4 | `host_exec_rejects_invalid_cwd_argv_timeout_concurrency_and_role` |
| S3.A5 | `host_command_timeout_kills_the_process_group`、`interrupted_host_command_is_recorded_unknown` |
| S4.A1 | `deploy_verify_passes_and_closes_as_deployed`：夹具 HTTP 服务 |
| S4.A2 | `deploy_verify_lists_mismatches_and_unhealthy_service_then_passes_after_fix` |
| S4.A3 | `deploy_verify_rejects_non_deployer_outside_run_pending_command_and_after_end`；`task_deploy` 已不在工具清单 |
| S4.A4 | CLI `human_deploy_verify_requires_running_runtime` |
| S4.A5 | `deploy_verify_runs_registered_command_with_artifact_env`、`deploy_verify_rejects_after_registration_change` |

原有部署职责测试改为经 `deploy_verify` 驱动，断言保持：成功与验收分开记录，尚未领取的投递被结束。依赖失败自报的两个测试删除；同一 L1 意图由 S4.A2 覆盖，即「核对失败时任务不关闭」。所有测试只使用临时目录与临时端口。

## 11. 风险与兼容

- **工作区升级不可回退。** 升级到 24 后，旧版本二进制打不开该工作区。这是首次迁移，只增加对象。
- **命令以本人身份运行。** 本机命令能调用 Atelier CLI、改写工作区数据库，也能访问家目录。逐条确认是唯一的闸门；`auto` 等于交出本人身份。这是 L1 R7 的明确取舍，不在实现里另加启发式拦截。
- **文件比对不能证明进程加载了新代码。** 需要这一保证的环境，改用核对命令，见 L1 第 9 节第 2 项。
- **核对命令以本人身份运行，不经确认。** 它由本人登记，部署成员不能修改；但部署成员可以通过本机命令改动它所检查的对象。
- **只支持 macOS 与 Linux。** 进程组信号依赖 `/bin/kill`。其他平台遇到时直接失败。
- **长时间等待确认。** 等待确认没有超时，部署会一直开放，由本人在 `pendingDecisions` 里处理。
