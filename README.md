# Atelier · 工坊

人和数字员工共同工作的地方。

Atelier 是一个独立项目，面向人和数字员工的持续团队协作。它以任务契约组织工作，将讨论、委派、执行、交接、检验和验收连接成可观察的交付过程。

Atelier 以 Worker 统一表达 Human Worker 和 Agent Worker（数字员工），以 Task 组织工作，以 Team 组织持续交付。每个 Team 必须有且只有一名团队负责人，由人或数字员工担任，成员按职责直接协作。任务契约、产出、检验与验收分别保留记录。 Task 可交给一个 Worker 或一个 Team；当前对话最多聚焦一个 Task，切换不结束原工作，成员与团队可以持续承担多项任务。

首阶段提供 Rust 核心与 CLI、Atelier Skill，TypeScript 用于 milkie 接入；GPUI 桌面界面后续补充。milkie 提供底层 agent runtime，通过进程协议接入；Atelier 保有工作与协作状态，并为不同 coding agent 保留能力边界。

## 当前状态

已开始 Issue #1 的基础实现：Rust CLI 可初始化工作区，保存成员与团队，提交任务及持久消息，补齐 pending 契约，并由人类团队负责人作承接决定。固定 Git 输入和检验配置可导入并绑定 pending 契约；普通待决定事项可正式回应并关联实际落实操作；请求去重、版本校验和数据库线程桥接已有集成测试；运行服务的单实例启动、独立存活、状态和停止已可用。连接版本、Worker 执行配置和 Task 冻结快照可保存；Run 领取/额度/故障通知账本及部分成员工具门禁已有核心测试；数字员工可通过绑定工具查询和正式回应补充事项、更新 pending 契约并核对实际落实操作；承接和协调处理结果可保存，投递须在资源确认停止后才结束。

TypeScript API 接入已开始，固定依赖构建和运行机制测试见 [milkie 适配](adapters/milkie/README.md)；生产服务已按持久投递启动独立 API 接入进程，装配冻结身份、工具与原生上下文，并核对停止/重启后的资源。真实模型完成业务和完整团队验收仍未通过。

API 凭据已支持 macOS Keychain：`connection credential set <连接 ID> --revision <当前连接版本号> --stdin` 从管道读取，需带全局 `--request-id`；不接受秘密参数，也不回退到文件或环境自动发现。`--version <连接版本 ID>` 可显式修复已冻结的旧配置；`connection credential clear` 清除选定版本的本地凭据，不撤销提供商侧令牌。设置成功仅说明本地保存。`connection test <连接 ID> --revision <当前连接版本号>` 配合全局 `--request-id` 发起有界 API 检查；`--version` 可检查同连接的旧冻结版本。检查只用最小无工具请求，不创建 Task/Run；结果包含版本、凭据代次和固定错误类别，不回显模型正文。`connection show` 展示当前 `readiness` 与最近检查的适用性。原 requestId 只查询/重放记录；要再次探测，使用新 requestId。

执行成员的产出提交与资源停止后的固定已有核心实现：固定内容、产出引用和结果消息保持事务边界，partial 与完整产出区分；`artifact show <ID>` 可查询。API 服务现会在子进程停止和成员操作排空后固定候选；独立核心测试仍包含故障注入，尚无真实模型交付成功证据。

成员文件工具已支持受控的候选列表、文本读取、原子写入和单文件删除；候选从冻结输入准备，检验成员只能读取交接产出。越界路径、链接访问、提交后写入和旧快照发布被拒绝，文件变更与调用账本同时提交。这些工具已接入生产 API 私有通道；真实模型形成产出的验收仍待完成。

已有固定产出可由团队负责人通过 `task verify <Task> --revision <版本> --artifact <ID> --instruction <说明>` 明确送检；执行成员也可按交接授权提交直交意图。接收/拒收持久保存，接受本身不产生检验通过结论。核心检查入口已能运行冻结的离线 Docker 检查，保存原始证据和隔离配置，再结合检验成员建议形成结论；执行/返工成员也可调用 `run_check` 对当前候选自测；结果绑定请求时的候选版本，修改后须重新检查，不能用于提交独立检验。`check show` 与 `verification show` 可查询，真实团队路径仍待接入。

独立检验为 `inconclusive` 时，团队负责人可用 `task verify --inconclusive <检验 ID>` 或成员 `task_arrange` 的 `verificationId` 明确重新交接。核心保留原终局和交接历史，核对资源停止后创建新投递；必须重新接收、运行新检查，不能复用旧证据。明确失败仍须返工，重检计总 Run 额度。

`mailbox retry <投递 ID> --revision <投递版本> --reason <修复依据>` 可由本人或获准团队负责人显式重新评估受阻投递，写操作要求 `--request-id`。数字负责人使用受控 `mailbox_retry`。已停止的未完成协调/检验沿用原账本并在新 Run 继续；已经受理的代码执行要走返工，未知资源须先核对，终局不重新调用模型。queued 不代表环境已可用，也不重置额度。负责人失效会原子建立本人恢复待办。使用 `task decision respond <事项> --revision <版本> --answer retry|wait|cancel` 保存选择，再用 `task recovery apply <事项> --revision <版本>` 落实；失败保留 `responded` 和 `blocked_reason`，选择本身不授予权限或解除 unknown。

`runtime reconcile` 在服务未持锁时独占核对旧 API 进程组与检查容器，不领取消息。返回 `stopped` 或 `blocked_unknown` 及未核对 Run；缺少 PID 登记继续 unknown，不凭人工声明释放。服务在线时由服务核对，该命令拒绝与服务并行。

人工验收已提供 CLI 与数字团队负责人的受控请求工具：请求固定契约、产出和最新独立检验，本人正式接受后才关闭。拒绝保留记录并通知团队负责人，同一产出不能换请求 ID 重新验收；必须明确返工新版并独立重验。`artifact export` 可取出指定当前、历史或部分版本，先核对内容，再发布到新目录或空目录，不改变验收状态。真实 Docker/CLI 和合成成员集成已验证，尚非完整产品验收。

产品 Atelier Skill 已提供最小宿主入口与按身份装配的成员指导。`skill install --destination <新 Skill 目录>` 安装宿主入口，相同内容重复调用不改写；已有不同内容或链接目标拒绝。宿主加载后调用 `skill describe --protocol 2`，可指定互斥的 `--team <ID>` / `--task <ID>`，由核心按本人、当前授权和冻结职责返回指导与操作索引。安装不创建工作区、不启动团队，不代表宿主已加载。成员工具说明从当前工具登记生成，执行/检验不会收到负责人完整操作路径。

完整异步团队尚未完成：两种 agent CLI 的隔离接入及登录/检查、完整恢复真实验收、Skill 的完整真实团队路径和真实模型业务仍待完成。`doctor` 区分服务状态、API 支持、已有 Skill 入口与未实现的 agent CLI；支持 API 不等于当前配置可用或任务已验收。开发进度与验收状态见 [实现记录](docs/implementation/1-first-team-delivery.md)。

## 运行基础 CLI

构建 CLI 需要 Rust 工具链，依赖版本由 `Cargo.lock` 固定。完整测试还需要 Node，以及按 [milkie 适配文档](adapters/milkie/README.md) 准备的固定源码包；Rust 的跨进程测试会启动构建后的 TypeScript 通道客户端。先准备依赖并构建适配，再运行完整检查：

```sh
node adapters/milkie/prepare-dependency.mjs /path/to/milkie
npm --prefix adapters/milkie ci --no-audit --no-fund
npm --prefix adapters/milkie test
cargo build --locked
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo run --locked -- --help
```

使用专门的空测试目录；初始化不会覆盖已有内容：

```sh
cargo run --locked -- --workspace /tmp/atelier-demo workspace init --name 本人
cargo run --locked -- --workspace /tmp/atelier-demo --json workspace show
cargo run --locked -- --workspace /tmp/atelier-demo --request-id worker-001 worker create --name 执行成员
cargo run --locked -- --workspace /tmp/atelier-demo worker list
```

运行服务通过 `runtime start/status/stop` 控制，按已有投递唤起 API 数字员工；人类待办不产生 Run。缺配置或凭据先 blocked 并通知本人，不消费 Run；已领取的处理失败不会自动重派。`runtime stop` 不取消任务，资源无法确认停止时保留 stopping/unknown。API 子进程使用同一应用安装目录下构建的 milkie 接入及 PATH 中的 Node，不继承宿主凭据环境。

已承接任务可由团队负责人调用 `task execute <Task> --revision <版本> --instruction <说明>` 保存首次执行安排；数字负责人通过绑定工具作同一决定。本机本人不能冒充数字负责人。返回 queued/blocked 只表示持久安排，缺配置或授权保留原因；服务可启动满足条件的 API 成员，agent CLI 尚未接入。已有安排不可换 requestId 重复创建，首次执行受理后的重做须走返工。

`task rework <Task> --revision <版本> --verification <失败检验 ID> --instruction <说明>` 保存返工安排；已核对的执行失败改用 `--failed-run <Run ID>`，两种原因只能选一种。仅冻结团队负责人可安排，数字负责人使用 `task_arrange` 的 `rework`。核心绑定前次执行与固定输入，安排时预留返工额度，领取才消费；重复请求不重复预留或消费，取消未领取任务释放预留。`task show` 返回 `reworks_used`、`reworks_reserved` 和 `rework_arrangements`。新产出必须重新独立检验，旧检验不可复用。

成员可用 `task_report_blocker` 指定处理者并报告阻塞，核心保存通知和停止请求；资源未核对前仍占用 Run。本人使用 `task blocker show <ID>` / `task blocker list --task <Task>` 查询，以 `task blocker resolve <ID> --revision <阻塞版本> --task-revision <任务版本> --evidence <原范围修复依据>` 正式解决；指定且获准的协调成员可用 `blocker_resolve`。解决不自动启动：执行阻塞由团队负责人用 `task rework --blocker <ID>` 继续，检验阻塞用 `task verify ... --blocker <ID>` 重新交接同一产出。返工的 `--verification`、`--failed-run`、`--blocker` 三选一；这些操作均需写请求标识。空候选的阻塞执行可在资源核对后结束，不生成虚假产出。

`sample prepare --destination <新目录或空目录>` 可在没有工作区时准备井字棋缺陷基线，返回固定 Git commit，不创建任务。样例故意漏判对角线；[可信浏览器检查](trusted-checks/tic-tac-toe/README.md) 放在候选目录之外，用于后续真实交付的独立检验。

固定输入通过 `input import --repository <本地仓库> --commit <完整 SHA>` 导入；只读取该 commit，忽略未提交和未跟踪文件，拒绝符号链接和子模块。返回输入 ID，用 `task update --code-input <ID>` 绑定。检验配置通过 `profile import --file <JSON>` 导入，返回 ID，用 `task update --verification-profile <ID>` 绑定；代码场景承接要求两者齐全并校验固定内容，承接后不可替换。示例配置：

```json
{
  "name": "井字棋独立检查",
  "check_id": "tic-tac-toe-browser-v1",
  "image": "sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133",
  "argv": ["node", "/checks/check.mjs", "/candidate"]
}
```

上述镜像是本地构建校准的具体版本；使用前应按可信检查文档构建并核对实际摘要。配置导入不自动构建或拉取镜像，不证明运行环境就绪。可用 `input list/show`、`profile list/show` 查询保存的版本。

`connection create --name <名称> --file <JSON>` 保存连接，`connection update --revision <版本> --file <JSON> <ID>` 发布新版本。API 示例：

```json
{"transport":"api","protocol":"openai-chat-completions","model":"模型标识","base_url":"https://example.invalid/v1"}
```

agent CLI 配置改用 `{"transport":"agent-cli","runtime":"pi"}`；两类字段不能混用，配置文件不能含密钥。API 已有凭据管理和连接检查；agent CLI 已有 connection prepare/login 管理入口，要求固定镜像、明确出站域名及 Worker 专用环境。首次登录需要交互终端，不能带 --json；材料保存不等于账号或模型可用，真实 CLI 业务执行尚未接通。Worker 创建或更新时用 `--connection <ID>` 显式绑定当前连接版本；连接更新后原绑定保持不变。pending Task 通过 `task update --refresh-team` 显式刷新快照，已承接任务不受默认配置后续变化影响。

`task decision request/list/show/respond/record` 提供补充资料或取舍的正式流程：request 指定问题、影响和处理者，重复 `--option` 提供可选项，省略选项则接受文本。respond 只保存答复并给发起者投递结果，不修改任务或授权。落实变更时在 `task update` 或 `team permissions update` 传 `--decision <ID>`；随后 record 使用 `--operation-actor <Worker ID> --operation-request <requestId>` 核对真实操作。无需变化用 `--no-change <原因>`，落实受阻用 `--blocked <原因>`；后者保留待处理状态。普通消息、通用邮箱处理及这些补充事项命令均不能代替人类验收。

`task acceptance request <Task> --revision <版本> --artifact <Artifact> --verification <Verification> --summary <说明>` 由冻结团队负责人提出；数字负责人使用 `acceptance_request`，并用 `message_respond` 记录等待本人决定。本人使用 `task accept` 或 `task reject`，同时提供 `--revision`、`--request <验收请求>`、`--request-revision` 和 `--reason`；这些写操作照常要求 `--request-id`。`task decision show` 查看请求，`task acceptance show` 查看已提交的接受/拒绝记录。拒绝后返工使用 `task rework --rejection <验收请求 ID>`，与其它返工原因互斥。`--decision-ref` 只保存脱敏人类决定引用，不授予权限。

`artifact export <Artifact> --destination <目录>` 要求目标父目录已存在，目标为新目录或空目录且不在工作区内部。它只复制指定产出的文件，不导出数据库、凭据或会话，也不提交任务操作，因此不要求 `--request-id`。结果包含产出摘要、文件清单和 `partial` 标志；再次导出到同一非空目录会拒绝，可改用另一空目录。失败不覆盖目标已有内容，掉电可能留下未发布的临时副本。


各子命令通过 `--help` 查看参数。核心对象变更须传稳定的 `--request-id`，更新须提供查询得到的 `--revision`；请求超时后先用 `request show <request-id>` 核对，相同 ID 和相同内容重试不会重复提交。CLI 始终代表本机本人，不提供自选 actor 参数；数字员工团队负责人的承接不能由管理 CLI 代做。数据库位于指定目录的 `atelier.sqlite3`，普通工作记录按本机可信用户边界保存，不用于存放凭据。

## 文档

- [产品方向与研究背景](docs/overview.md)
- [L1 产品设计：首项异步团队交付](docs/design/1-first-team-delivery/product.md)
- [L2 技术设计：首项异步团队交付](docs/design/1-first-team-delivery/technical.md)
- [名词表](docs/glossary.md)
- [仓库协作规则](AGENTS.md)

## 首个验证场景

人把一项代码修改目标交给 Team，由唯一团队负责人组织承接并明确任务负责人，数字员工执行、另一名数字员工独立检验，人验收交付。验证包含成员直接交接、阻塞处理及检验失败后的返工；任务不会因会话或单次执行结束而失去状态，检验对应当前版本，人无需逐条转发消息。

首项交付以 CLI 和 Atelier Skill 为入口，建立本人、数字员工团队负责人、执行成员、检验成员与 Team。任务提交到负责人的持久收件箱，成员异步处理工作消息、交接、检验和有限返工，最终由人验收。工作区运行服务显式启动后独立于宿主会话运行，首版串行唤起成员；CLI 提交安排返回投递状态，不前台承包整个交付。Task、工作消息、成员 Run 和验收分别留存，核心强制权限和版本规则。

GPUI 桌面入口属于后续范围，方向见 [产品 overview](docs/overview.md)，进入对应 feature 时再编写 L1/L2。上述完整团队路径仍是目标契约，当前基础 CLI 不代表该路径已实现或通过验收。
