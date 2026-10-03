# Issue #1 实现记录

日期：2026-10-03。模式：end-to-end；当前阶段：implementation；状态：进行中，未完成交付、未送独立审查。分支 `feat/1-first-team-delivery`；当前成果保存为开发检查点，尚未冻结验收候选；以下按顺序保留各开发阶段的历史记录。关联 [Issue #1](https://github.com/xforce-io/atelier/issues/1)。

设计依据为 [L1 v0.10](../design/1-first-team-delivery/product.md) 与 [L2 v0.10](../design/1-first-team-delivery/technical.md)，均为 2026-10-01 修订。L1 复用此前逐轮讨论确认的产品范围；用户在 Issue 同步后回复“go”，本次据此进入开发。L2 补齐 pending 配置刷新、消息处理终局、跨 Run 操作去重的规则；未自行批准设计或减少验收。`keel-how` 按“无现成机制可讲”跳过。

## 已有实现

- TypeScript API 单次执行适配已按固定 milkie 提交构建；通过真实 Runtime 检验工具白名单、原生 checkpoint 和持久调用账本，私有进程通道已接到 Rust 成员入口并通过真实子进程测试；生产服务已装配并启动独立 API 执行进程，真实 CLI/Keychain/Node 启停与强杀恢复已有 Integration，尚无真实模型业务成功。详见 [API 接入](../../adapters/milkie/README.md)。
- Rust 单 crate 提供核心库和 CLI；依赖由 Cargo.lock 固定，第一方代码禁止 unsafe。
- SQLite 保存本人、数字员工身份、团队及显式授权、任务、工作消息、投递和请求结果；写事务采用 IMMEDIATE，2 秒锁等待上限，foreign_keys 开启、synchronous=FULL。任务创建、消息和投递、请求结果同事务提交。
- 工作区初始化要求新目录或空目录，独占创建数据库文件；读取缺失/损坏/不支持格式的工作区不会自动重建。
- CLI 提供 skill install/describe、workspace init/show、worker create/update/list/show、team create/update/list/show、team permissions update、task create/update/intake/execute/verify/rework/cancel/list/show、task decision request/list/show/respond/record、task acceptance request/show、task recovery apply、task accept/reject、artifact export、task blocker list/show/resolve、mailbox list/respond/retry、message send、request show、runtime start/status/stop/reconcile、sample prepare、input import/list/show、profile import/list/show、connection create/update/test/list/show/version、run/artifact/handoff/check/verification show、connection credential set/clear、doctor。JSON v2 单结果；写操作必须带 requestId，更新带 revision。
- CLI 始终代表本机本人，不能传 actor 冒充数字员工；团队负责人为数字员工时，本人不能代其承接。人类承接后留下持久 result 投递，继续安排责任不会因为承接命令成功而消失。
- API/agent CLI 连接采用互斥字段并保留不可变连接版本；Worker 显式绑定形成不可变执行配置。修改连接不会静默替换已有 Worker 配置；pending 显式刷新才更新 Task 的职责、工作说明和执行配置快照，已承接任务保持冻结。当前保存不代表环境可用；API Keychain 凭据已实现，API 连接检查已有最小请求、持久诊断和失败/重放验证，真实模型成功仍未取得；CLI 登录/检查待实现，API 已保存并装配跨 Run 上下文绑定。
- requestId 绑定规范请求摘要，同 ID 同内容返回原结果，不同内容冲突；重试和结果查询先检查当前权限。更新以显式补丁为请求内容，避免重试时从最新状态填默认值导致摘要变化。
- 固定 Git 输入直接读取完整 commit 的对象，校验路径、类型和 50 MiB 总量；未提交修改不会进入任务。逐文件内容先持久化，再原子发布输入记录和请求结果。已提交请求在原仓库移走后仍可重试；事务失败只允许遗留未引用内容。
- 检验配置固定镜像摘要和命名 argv，拒绝可变镜像 tag 与未知字段；保存不宣称环境就绪。pending 代码契约可绑定输入和配置，承接校验齐备及内容完整性后冻结。可信同步管理入口在事务前核对内容；数字员工代码承接通过数据库桥接取只读准备信息，再由阻塞工作线程核对文件，发布事务重新核验身份、任务版本与固定输入。核对不占用事件循环或数据库线程。
- 普通待决定事项与消息同事务保存；正式回应保持 responded 并原子投递给发起者。业务操作通过 decision 显式关联，record 核对实际请求账本与版本后才 resolved；无变化需原因，受阻保留原因与责任。其他任务版本更新、承接或取消将旧事项过期；普通消息/邮箱处理不能冒充正式回应。本机管理入口与绑定的数字员工入口均已实现普通补充事项；验收事项已接入专用请求/决定入口，恢复事项已有专用选择/落实及原子通知，详见末节。
- 普通消息只允许 note/question，只能发给任务参与者；消息预算持久保存，新操作的相同文字仍消费额度。Run 预算已与 prepared 登记原子提交；返工安排已预留额度，领取时与 Run 额度一并消费。
- Run 账本将领取、冻结职责/当前权限校验、配置绑定、投递关联及额度消费放在同一事务，数据库唯一索引限制全工作区一个 prepared/running/unknown Run。启动意图先持久化；旧 epoch 拒绝写入；重启后未启动资源的 prepared 可停止，已记录启动意图的 Run 保持 unknown，直到可信资源核对。撤权、取消和停服持久请求停止，恢复授权不复活旧 Run。
- 核心失败通知与 Run/投递状态同事务保存，按 Run/接收者去重；标记 core 来源，无业务操作者时 sender 为空。普通消息标记 worker 来源，业务通知保留真实操作者。消息预算不丢弃失败通知；Run 额度耗尽后仅生成人类通知。数字负责人失效现在生成正式恢复待决定事项；S5.A13 完整真实路径仍待验收。
- 成员工具入口仅由可信服务创建运行绑定，无 CLI actor 参数；当前目录包括 task_read、decision_read、message_send、decision_request、decision_respond、decision_record、task_update、task_intake、task_arrange、message_respond、artifact_submit、handoff_read、handoff_offer、handoff_respond、run_check、check_read、verification_submit、list_files、read_file、write_file、delete_file、task_report_blocker、blocker_resolve、acceptance_request 等工具，按绑定职责和权限提供子集。每次调用及重放先检查 epoch、Run/投递归属、任务版本和当前授权；deliveryId/operationId 与原始工具调用标识持久去重，业务效果和结果账本同事务。成功和业务拒绝均计入每 Run 100 次调用上限，数据库故障可按原调用重试。该入口已由私有管道调用并经过跨进程测试，尚不代表真实模型已能完成任务或结束投递。
- 数据库桥接使用专用线程、有界 Tokio 通道和单次回复；取消等待不撤销已受理操作，关闭时停止接收并处理完在途请求。常驻运行服务通过该桥接读取停止请求与队列状态。

运行服务使用 OS 文件锁保证单实例、独立进程组与空标准流脱离发起终端；每轮 500 ms 检查停止请求、旧资源及已有持久投递。API 数字员工按冻结配置和本地 Keychain 引用启动独立 Node 进程，私有管道绑定 Run/Task/Delivery，协调和检验不准备可写候选。未知 Run 占用名额；停服请求不取消任务，未核对资源继续 stopping。人类投递不启动模型，缺配置/凭据阻塞并通知本人，不扣 Run。负责人自身失败不产生新的自我唤起。状态支持 API 不代表端点可用或业务已完成；agent CLI 与完整真实恢复路径仍待实现/验证；服务控制直接查询实际状态。

构建和基础 CLI 用法见 [README](../../README.md)。本阶段数据库格式尚未发行，新增连接、Run、消息来源、成员调用账本及产出/交接/检查/检验、候选清单、凭据引用及上下文/接入资源记录、连接诊断与自测输入快照及返工安排、正式阻塞、交接替代与验收决定后格式为 schema 21；旧 schema 1–20 被明确拒绝，不自动重建或覆盖，无历史产品数据迁移承诺；测试只使用临时工作区。

## 开发检查与证据边界

环境：macOS，本机 Rust 1.91.1 / Cargo 1.91.1。常规测试不访问真实模型、业务目录或私有会话；另行运行的 Keychain/Node 测试使用合成凭据与无效端点，真实 API 尝试的失败单独记录。

| 检查 | 结果 | 证明范围 |
|---|---|---|
| `cargo test --locked --all-targets` | 通过，135 个测试；另 7 个 Docker 和 3 个 Keychain 测试默认跳过 | 19 个 CLI、111 个核心/数据库桥接/私有通道集成测试、5 个 Unit |
| `cargo test --locked --test core real_docker_ -- --ignored --nocapture` | 历史 6 项通过；新增 CLI 回收场景本轮单独通过，未重跑全 7 项 | 真实冻结检查、核心结论、容器恢复、自测、失败/人类拒绝后返工及验收/导出、不确定检验后的新交接；成员操作和候选由测试构造 |
| `cargo test --locked --test core real_keychain_ -- --ignored --nocapture` | 通过，2 个测试 | 真实 Keychain 和实际 CLI 的受保护凭据设置、重试、旧版本修复与清除，全部为合成秘密 |
| `cargo clippy --locked --all-targets -- -D warnings` | 通过 | 当前编译目标的静态检查 |
| `cd adapters/milkie && npm test` | 通过，25 个测试；固定依赖此前已完成 `npm ci` 重装验证 | 真实固定版本 milkie Runtime、文件账本、原生 SQLite/checkpoint；模型响应为 fixture，不是真实模型验收 |
| `cargo fmt --check` | 通过 | Rust 格式 |
| `git diff --check` | 通过 | 工作区文本差异检查 |

测试源码为 [tests/cli.rs](../../tests/cli.rs) 和 [tests/core.rs](../../tests/core.rs)。覆盖多进程并发同请求只有一个 Task/投递、消息写入故障整体回滚、连接重开后 queued 和请求结果保留、过期决定、数字员工身份不可冒充、承接后继续责任、冻结快照、消息预算、撤权后缓存拒绝、数据库线程取消和背压、样例固定 Git 基线与不覆盖保证，以及服务独立存活、并发启动、强杀重启与停服保留队列。新增固定输入/配置测试还覆盖脏工作目录隔离、源仓库移走后重放、拒绝符号链接/可变镜像/未知配置、承接时损坏输入拒绝及冻结、发布失败无记录和恢复重试。待决定事项另覆盖真实 CLI 问答到落实、普通文字不闭合决定、越权/过期拒绝、记录受阻、伪造落实依据拒绝、实际变更与记录分离、撤权后的重放拒绝，以及回应/结果投递原子回滚。消息、决定结果与输入发布写入故障通过测试数据库触发器注入，其他成功数据库路径使用实际 SQLite；它们不证明模型执行、容器隔离或真实 Skill 已可用。

连接配置另覆盖互斥字段、拒绝秘密字段、不可变绑定和显式刷新。Run 测试覆盖并发领取、账本/投递/额度回滚、冻结角色与权限分离、旧 epoch、撤权不复活、取消等待停止、unknown 不回 queued，以及失败通知去重和额度耗尽后的人类通知。成员工具测试覆盖身份/任务不可替换、管理工具不可调用、真实业务通知归属、丢回复重放、同调用更换 ID 拒绝、撤权后缓存拒绝、决定/消息/调用账本整体回滚及错误调用额度。Run 的启动与资源停止观测在这些核心测试中由测试注入，并未启动模型或核对真实执行容器。

上述是实现者当前工作区检查，不是冻结候选上的产品验收。全部 58 项 L1.8 验收仍须在完整实现和具名真实环境中取证；没有把局部测试汇总为 Story pass。

| Story | 当前状态 | 主要功能地图与未完成范围 |
|---|---|---|
| S1 | not_run | setup-and-intake：基础对象、授权和不可变连接配置已有测试，API Keychain、服务接入进程及 API 连接失败诊断已有真实测试；隔离 CLI 登录/检查未实现，Skill 安装/描述与基础宿主观察已有；真实模型业务尚未通过 |
| S6 | not_run | task-intake：任务/消息原子保存、人类承接及数字员工承接工具已有测试；普通待决定事项已有 CLI/核心测试；数字员工落实决定与恢复类事项已有核心测试，真实模型承接与完整产品 Skill 路径尚未验收 |
| S2 | blocked | code-delivery：首次执行安排、Run 领取账本及部分成员工具门禁已有测试；候选文件工具、提交与固定产出已有核心测试；API 服务接线和资源核对已有 Integration，真实模型业务与两种隔离 CLI 尚未通过；接入依赖见下文 |
| S3 | blocked | verification-rework：交接及接收/拒收已有核心测试；核心独立检查与恢复已用真实 Docker 验证；返工续接与模型路径未完成 |
| S4 | not_run | human-acceptance：验收/拒绝后新版继续及产出导出已有核心和真实 CLI/Docker Integration；真实模型团队、Skill 和实际游玩仍未验证；测试中的人类决定为 fixture |
| S5 | not_run | failure-and-lifecycle：存储/权限/请求部分已有测试；服务进程生命周期已有测试；Run 核心状态和故障通知已有测试；API 子进程与检查容器资源核对已有 Integration；正式恢复待办/落实和离线核对已有 Integration，完整模型/Skill 恢复未验收 |
| S7 | blocked | asynchronous-team：无真实数字团队闭环和完整产品 Skill/成员执行证据，不能取团队闭环证据 |

功能文件目录为 [验证地图](../../.agents/skills/verify-atelier/features/README.md)。未列为 pass 的项仍是必需范围；未运行不是不适用。本记录尚无冻结验收候选 SHA；开发提交不等于验收候选，不可据此送审或发布。

## 接入依赖与下一步

只读检查了相邻 milkie 工作区，origin 为 `xforce-io/milkie`，当前分支 `feat/263-agent-cli-execution`，HEAD `7865ffcc14a8359a055e5e6e0998b56ab2160379`。`src/execution/` 为该工作区未跟踪开发代码，连接代码也有未提交变更；本任务未修改它们，不能把 HEAD 当作这些接口已发布的版本。

重新核对后区分两条路径：已提交的 API AgentRuntime 支持关闭全部内置工具、自定义受控工具、I/O 边界稳定调用 ID 与原生 checkpoint，因此 API 适配可独立推进。当前本地 `src/execution/types.ts` / `ExecutionClient.ts` 接口提供输入文本、read-only/standard 工具策略、超时、上下文与取消。`start` 明确拒绝其他约束字段；尚无 Atelier 需要的获准工具清单/回调、私有成员工具请求通道、运行前 Skill 装配及外部隔离环境控制契约。`adapters.ts` 的 standard 策略还允许原生写入或 shell，不能拿来替代 L2 的候选访问和权限边界。

API 已固定提交 `7865ffcc14a8359a055e5e6e0998b56ab2160379`，从干净 Git 快照两次构建得到相同包摘要 `6a4de9e52702ebc2b511b68af810743637d29817b17fa41090f7186267a88eec`；不使用缺白名单能力的已发布 NPM 0.1.1。下一步为生产服务装配 API 进程、提供凭据并核对资源；CLI 仍需确定受控工具/隔离环境接口并真实验证，缺能力应拒绝启动。与此同时，Atelier 自身仍需完成凭据与连接检查、Run 的实际进程接线与资源核对、产出/交接/检验/验收、恢复及验收待决定事项、产品 Skill，以及样例到真实 Artifact 的完整接入，这些不能全部归因于上游依赖。

本次没有合入、发布或真实产品验收。后续须完成剩余实现、冻结候选、逐项 CLI/Skill 验证，再进入独立审查；现有测试不得直接当成 Issue #1 已完成。

## 井字棋验收资源（进行中）

`sample prepare --destination <空目录>` 不依赖工作区，生成只有公开合成输入的 Git 仓库；当前 `tic-tac-toe-defect-v1` 基线 commit 为 `c9df4f0f7ba0221545c9fc6c309b8d3960b148e7`，两条对角线故意漏判。独立目录生成相同 commit、不覆盖已有文件已通过 CLI 集成测试。

可信检查资源见 [trusted-checks/tic-tac-toe](../../trusted-checks/tic-tac-toe/README.md)，19 个浏览器场景，Playwright 1.56.1 与 npm lock 固定；基础镜像实际 RepoDigest 固定为 `mcr.microsoft.com/playwright@sha256:f1e7e01021efd65dd1a2c56064be399f3e4de00fd021ac561325f2bfbb2b837a`。已在离线、只读根、非 root、1 CPU／512 MiB／64 进程上限下完成校准：缺陷基线 15 项通过、4 个对角线场景失败；临时参考 19 项通过。使用 Chromium 单进程模式减少进程开销，缓存仅写入既有受限 /tmp，未提高资源限制。不能把检查器校准当成成员交付或产品验收。

本次实际检查镜像 ID 为 `sha256:7a87e3fe2909d0135d9041760b2808e70b9d2afb368e450e01d96b614665d133`；原始校准证据保存在 `.agents/verify-runs/1/tic-tac-toe-calibration-1790869334012.json`。最初系统临时目录挂载失败和默认 Chromium 线程耗尽均为检查器运行失败，未记为候选通过；调整了校准目录与浏览器进程模式后重新构建镜像并重跑。

## API 接入验证（2026-10-02）

新增 `adapters/milkie`，Node 23.11.0 / npm 10.9.2、TypeScript 5.9.3。固定源码包由独立 Git archive 构建，上游工作区未修改。17 个测试验证工具仅在合法派发后转交核心、稳定调用 ID 内容冲突、回复丢失停止模型、重开先核对原操作、撤权结果重查、新 Run 新调用不语义合并、取消、原生 checkpoint 重开后新工具范围以及 100 次工具调用上限。

初次工具预算测试发现：第 101 个工具被底层拒绝后，milkie 仍可能进入后续上下文准备并以 context budget 错误结束。适配边界新增明确计数与停止，保留原始 milkie 终态，同时报告 `TOOL_CALL_BUDGET_EXCEEDED`；未增加任何预算。传输失联或取消中的核心操作保留待核对记录，恢复后先查原请求，不能以重新调用模型代替核对。

自动化套件使用模型 fixture；另已尝试真实 API 集成，因服务端订阅错误失败，见下节；尚未接入 CLI 容器，不是完整成员执行或产品 Skill 的通过证据。该轮时服务仍报告 executionSupported=false，后续生产接线见本文末节。模型 fixture 只用于确定性故障和运行机制验证，不替代真实成功链路。


## 私有通道与 API 进程（2026-10-02）

新增 Rust 通道服务与 TypeScript 管道客户端，身份以绑定 Run 为准；校验协议版本、作用域、能力与 Skill 摘要、双向连续序号及重复内容。工具调用走数据库线程，发送回复失败不回滚已提交操作。限制帧数/字节数、握手/写入/运行期限；终态只返回给资源所有者，不能自行改变 Task 或 Delivery 状态。

4 个 Rust 集成测试使用真实 Node 子进程和生产 TypeScript 通道客户端，覆盖正常/重复请求、跨任务请求、序号缺口/冲突、截断终态以及在核心提交后注入回复管道断开。Node fixture 选择确定性工具调用，不是真实模型；正常 terminal 后投递仍为 claimed，确认子进程退出且没有业务终局后才 blocked。

API 子进程入口已实现显式配置解析、私有上下文校验、milkie 原生存储/续接和持久工具账本装配。新增 TypeScript 测试覆盖目录及绑定隔离、checkpoint 缺失、禁用凭据环境回退的输入要求，以及通道停止/错误/大小限制。原生 Runtime 的模型响应仍为 fixture。生产运行服务尚未调用该入口，凭据、其余成员业务操作、资源恢复核对及真实模型验收继续待办。

完整 Rust 测试现在需要先按 adapter README 构建 TypeScript；不得依赖旧 dist 文件证明新源码通过。当前检查顺序为 `npm test`（含 build）→ `cargo test --locked --all-targets` → Clippy；全部执行于本机，尚无 CI 或独立审查证据。


## 数字员工承接与协调处理终局（2026-10-02）

`task_intake` 只允许同时具备冻结职责、Run 工具权限和当前安排授权的团队负责人。接受冻结契约并保留当前 claimed 投递的继续责任，不自动生成给自己的新协调投递，也不因承接成功而 handled。等待/拒绝保存正式决定及通知；全部仍须确认资源停止后核对投递。成员自己提交的合法承接推进本 Run 的任务依据版本；其他来源的任务版本变化仍拒绝旧操作。

`message_respond` 保存独立处理记录，允许明确等待原因与处理者、对原发送者的真实回复引用或有效待决定事项引用；承接和正式决定不能用普通聊天回复替代。等待通知、处理记录和工具请求结果同事务提交。已有处理结果后拒绝本轮新的业务写入，缓存重放仍先核对当前授权。`mailbox list` 展示 `handlingResult`，在 claimed/uncertain 时也可查，状态不会提前变为 handled。

资源确认停止时，核心用持久处理结果判断 handled；缺失则 blocked。任务取消或新版本使旧结果失效时，投递 cancelled，历史处理结果仍保留。terminal 丢失后只补核对已有记录，不再次调用模型。执行/检验用途不能使用协调回应绕过各自终局要求。

新增测试覆盖承接后静默退出、显式等待、正式等待/拒绝、普通回复的真实引用、伪造/过期引用、取消/更新、处理记录与调用账本原子回滚、终态丢失和重开。真实 Node 管道 fixture 另验证保存回应后依然 claimed，回收实际子进程后才 handled。代码承接使用真实固定 Git 输入，损坏时拒绝，修复后原调用成功，已提交的重放不重新读取外部内容。模型仍未实测，不据此勾选 S6/S7。


## 数字员工补充事项与真实 API 联通检查（2026-10-02）

普通补充事项的数字员工路径现可读取详情、由指定处理者正式回应、由原发起者更新 pending 契约并核对实际操作引用。`task_update` 的任务变化、Run 依据版本、普通操作账本和成员调用账本同事务提交；核心返回 operationReference，模型不能杜撰引用。数字员工更新不重复向自己投递承接通知，原 claimed 投递继续承担责任。等待结束后可以从新消息继续落实同一事项，不要求恰好停留在原来的 decision.result 投递；受阻不能用决定引用提前结束，须明确等待原因和处理者。

新增五个核心集成测试覆盖完整问答与落实、跨消息继续、角色与作用域校验、拒绝通过补齐操作修改配置/职责/额度，以及操作账本写入失败的整体回滚。核心工具目录按 Run 提供九种工具的适用子集和中文协作指导；这是 API 装配资料，产品 Atelier Skill 的安装、描述和真实宿主入口仍未完成。

新增开发集成入口 `cargo run --locked --example api_smoke`，要求 `ATELIER_SMOKE_WORKSPACE` 为新目录，并从当前进程读取 VOLCENGINE_TOKEN、VOLCENGINE_MODEL、VOLCENGINE_API_BASE。凭据仅经私有管道提供给清空环境的真实适配子进程，不存入连接配置或证据。该入口只让真实模型处理公开合成井字棋任务的澄清阶段，使用核心绑定工具；不是产品 CLI/Skill 验收入口，不串联后续业务阶段。

首次装配发现 milkie 显式 fields 与 env 输入不能同时传入，即使 env 为空；修复为仅传 fields，并保留禁用旧凭据回退。新增生产子进程回归测试验证真实 gateway 能装配、取消在访问服务端前生效且不输出秘密，17 项 TypeScript 测试通过。

修复后真实进程正常退出，模型请求却以 MODEL_BAD_RESPONSE 终止，未产生待决定事项；Task 保持 pending，投递 blocked，向本人保存失败通知。使用同一显式连接的最小合成请求进一步得到 HTTP 400 / InvalidSubscription。该连接需修复订阅或替换为可用连接；不盲目重试、不标真实模型成功。失败证据保留于本机 `.agents/verify-runs/1/api-smoke-1790875887.json`（装配失败）与 `api-smoke-1790876177.json`（服务端拒绝），均标记 productAcceptance=false。

当前 Rust 66 项与 TypeScript 17 项通过，Clippy 通过。生产运行服务仍未启用成员执行；全部 S 与 58 项产品验收状态不因本次局部实现或失败联通检查而升级。


## 首次执行安排（2026-10-02）

`task execute <Task> --revision <版本> --instruction <说明>` 与绑定成员工具 `task_arrange(action=execute)` 已实现。调用者必须是冻结团队负责人，同时满足冻结及当前安排/通信授权；Task 必须已承接且未取消。目标固定为任务执行成员，不能传入其他 Worker。安排、消息额度和请求结果同事务保存，不启动进程、不消费 Run 额度，也不改变契约版本。

执行成员配置或授权不足时保存 blocked 投递与原因；否则 queued，实际环境准备与 Run 受理仍另行判定。已有未取消的执行安排禁止通过新请求重复创建；首次执行已受理后，即使运行失败或停止，也不能再次调用 execute 冒充首次执行，后续须走返工。返工/检验安排及完整恢复入口尚未实现，不把拒绝重复当作这些路径已完成。

团队负责人处理消息时可用 `message_respond(kind=assignment)` 引用在本投递处理期间提交的有效安排；核对真实成员调用账本、发送者、Task 版本和安排状态，伪造、其他投递和 blocked 安排不能用来结束责任。blocked 需保存带处理者的等待依据。正式决定消息仍须回应或核对对应决定，不能用安排替代。有效处理结果保存后，当前投递保持 claimed，资源确认停止后才 handled；执行成员投递独立保留。

新增 6 个核心测试和 1 个真实 CLI 测试，覆盖持久化/重放、两个数据库连接不同请求的并发唯一性、业务结果与账本故障回滚、当前权限重查、人类管理身份不得冒充数字负责人、未承接拒绝、blocked 等待责任，以及数字负责人承接后安排并明确结束本消息。运行资源由测试注入，不是实际模型执行。当前累计 73 个 Rust 测试通过，TypeScript 17 项此前通过，产品验收仍未通过。


## 产出提交与停止后固定（2026-10-02）

执行成员的 `artifact_submit` 只保存产出说明与提交引用；来源固定为当前执行/返工 Run，协调或检验 Run 无权提交。提交后禁止新的业务写入，已提交调用可按原标识核对。Task 此时没有新增 currentArtifact，执行投递仍为 claimed。尚未实现的交接意图不隐式生成，当前提交不会自动送检。

可信资源所有者在确认候选写入者、子进程/容器及在途核心调用全部停止后，调用固定入口；该确认仍由后续生产服务资源管理实现，当前测试注入观测，成员和管理 CLI 没有捷径。候选路径由核心工作区与 Run ID 推导，模型不能选择宿主路径。目录遍历拒绝符号链接、硬链接、特殊文件、不安全路径、超过 10000 个文件/目录或 50 MiB 的内容；文件先存入核心内容地址对象并同步，再在短事务中发布引用。文件读取和摘要计算通过 blocking 工作线程完成，不占数据库线程或事件循环。

发布事务重新核对当前契约、取消及授权状态；产出引用、currentArtifact、给团队负责人的结果消息、投递处理记录和 Run 停止同事务提交。没有正式提交的内容为 partial；取消或撤权后只保留 partial 历史，不替换当前产出、不生成可交接成功结果。Run 持久记录曾被撤权，即使授权后来恢复，也不会把旧执行恢复为完整产出。正式提交且依据有效时，完成固定后才能 handled；仍不代表独立检验或人类验收。CLI `artifact show <ID>` 查询固定版本。

丢失 terminal 后，新服务可核对实际资源停止，使用原提交和候选固定产出，不新建模型 Run。已发布的重复核对直接返回原 Artifact，不重新读取已移走的候选。发布失败允许留下未引用内容对象，但不会留下已成功的产出引用、结果消息或错误的停止状态。

新增 7 个核心集成测试，覆盖数据库桥接和真实文件固定、真实 CLI 产出查询、同调用重放、提交后写入拒绝、撤权再恢复、取消与 partial、丢终态恢复、通知写入故障整体回滚，以及符号链接/硬链接/socket/超限文件拒绝。首次 socket 测试受 macOS 路径长度限制失败，改为短路径创建后移动到候选，再完整重跑。80 个 Rust 测试及 Clippy 通过；这仍不是实际模型产出或资源隔离验收。后续仍须完成候选读写工具、实际服务装配、交接/检验/返工/验收/导出和产品 Skill。


## 显式交接、接收与拒收（2026-10-02）

团队负责人可通过 CLI `task verify <Task> --revision <版本> --artifact <ID> --instruction <说明>` 或绑定工具 `task_arrange(action=verify)` 发起交接；获准原执行成员可用 `handoff_offer` 交接自己的当前产出。接收者固定为独立检验成员，不能改派；必须是当前契约的完整固定产出，未固定、partial、其他任务或旧产出拒绝。未拒收的交接以 Task/Artifact/接收者唯一约束，不能换请求重复送检；缺配置/授权保留 blocked 原因。

执行成员还可在 `artifact_submit` 提供获准的 handoff 说明，保存明确直交意图。意图不会立即投递；资源停止且产出固定后，同事务生成交接与检验消息，没有该意图则仍由团队负责人决定。若预算等业务前置拒绝交接，固定产出保留并记录 handoff_error、通知团队负责人；数据库故障则产出引用、交接、消息和停止状态整体回滚。撤权后的旧 Run 不会因恢复权限而重新获得直交机会。

检验成员通过 `handoff_read` 查询说明，通过 `handoff_respond` 明确接受/拒收；身份绑定当前 verify Run 与交接投递。接受只改变交接记录，不产生检验结论、不结束投递；没有后续检验结果就退出仍 blocked。拒收保存原因并通知原发送者，资源停止后才 handled；原范围补充后可新建交接，历史拒收不覆盖。CLI `handoff show` 保留交接依据和决定。领取 verify 投递也检查当前产出依据，旧交接不能凭排队状态启动。

新增 5 个核心集成测试（其中使用真实 CLI 送检）：授权直交等待固定、无权直交后由团队负责人安排、接受不冒充检验、拒收与补充后重交接、重复/partial/跨任务拒绝、通知失败原子回滚、消息额度耗尽时保留固定产出。当前 85 个 Rust 测试通过；检查器执行、建议与核心结论、有限返工、实际模型与产品 Skill 路径仍待完成，S3/S7 不标 pass。开发数据库为 schema 10，未发行旧格式继续明确拒绝，不自动覆盖。


## 独立检查、检验结论与检查资源恢复（进行中）

检验成员接受交接后，绑定工具 `run_check` 只接收冻结的 checkId。核心先持久保存启动意图，再在数据库线程之外准备固定候选并创建独立 Docker 容器；模型不能选择镜像、命令、挂载或填写检查观测。启动前核对实际容器的无网络、非 root、只读根/候选、cap-drop、no-new-privileges、1 CPU/512 MiB/64 进程和受限 /tmp 配置，将必要参数保存为证据，不保存整个容器环境。

真实检查保存原始日志内容引用、结构化报告、退出码、版本与停止事实。报告与冻结 checkId、退出码必须一致；截断、缺证据、超时、停止未知均为 inconclusive。`verification_submit` 只能引用本 Run 最新且匹配当前固定产出的检查；失败证据不能被模型 pass 建议覆盖，通过证据仍需独立成员建议通过。检验记录、负责人结果消息与处理结果同事务；投递在其余 Run 资源停止后才 handled，任务仍等待后续成员决定及人工验收。CLI `check show` 和 `verification show` 可查询。

检查中停止 Run 会被拒绝；尚未启动的 prepared 检查可随 Run 停止记 inconclusive。服务启动会核对旧 epoch 检查，归属不符不删除，停止未知继续占用；核对清理后未持久保存的结果只能 inconclusive。清理检查容器不释放整个 Run，接入进程等其他资源仍须核对；不会启动新模型、自动重派或退还预算。

验证：默认 87 个 Rust 测试通过；另显式运行 2 个真实 Docker 测试通过，合计 89 个不同测试。缺陷候选 15 项通过/4 项失败，测试参考候选 19 项通过；即使 fixture 建议 pass，前者仍保存 fail。同调用重发复用证据，接受前检查和无证据建议被拒绝。恢复测试另确认归属不符容器保留、归属匹配后清理、旧 epoch 拒绝以及其他 Run 资源仍 unknown。Clippy、格式与差异检查通过。

本次检查证据：`.agents/verify-runs/1/core-docker-check-9211e3c7-1b44-41be-b453-99903765834b.json`。成员决定与候选由测试构造，`productAcceptance=false`；未验证真实模型团队、完整服务接线或 Skill，不勾选 S3/S5/S7。数据库现为 schema 11；历史小节的计数及格式描述保留当时事实。后续仍须完成候选工具、服务执行接线、凭据/隔离 CLI、返工、验收/导出和真实逐项验收。


## 候选文件工具（进行中）

核心通过 list_files/read_file/write_file/delete_file 提供本 Run 的文件视图，候选从冻结 Git 输入准备；检验 Run 只读当前交接产出，协调 Run 无候选访问权。文件名始终在清单内解析，不用于拼接模型可选的宿主路径；禁止越界、.git、符号链接及文件/目录路径冲突。读取按 UTF-8 边界分段，列表分页并限制实际 JSON 大小；二进制输入可保留，但文本工具不伪装成可编辑文本。

复用现有内容存储：文件先写入不可变内容对象并持久化，候选清单版本与成员调用结果在同一短事务提交。失败只可能留下无引用对象，不提前暴露修改；同调用重放复用结果，发布前重新检查当前授权和候选版本。文件准备及内容校验在阻塞工作线程，SQLite 线程只处理记录。候选固定再次校验全部内容，发布时核对清单版本，旧快照不能覆盖新写入。

新增 2 个集成测试覆盖真实输入准备、成员文件工具到固定产出和只读交接、写入/账本故障一起回滚、重试、删除、提交后拒写、旧快照拒绝、越界/链接拒绝、UTF-8 分段和撤权后缓存拒绝。默认 89 个 Rust 测试、另行运行的 2 个真实 Docker 测试及 Clippy/格式/差异检查通过，合计 91 个不同 Rust 测试。Docker 检查测试已改为使用成员文件工具形成候选后再固定、交接和检查；最新证据为 `.agents/verify-runs/1/core-docker-check-0e30af70-6c64-4f8c-8497-4c98cbc47433.json`。

成员调用和运行资源停止仍由测试构造，生产服务仍未启用成员执行；真实模型、返工安排及跨 Run 续接尚未验证，S2/S3/S7 不升级为通过。开发数据库现为 schema 12；历史计数保留当时事实。后续继续服务实际接线、受保护凭据、隔离 CLI、返工、验收/导出与产品 Skill。


## 受保护 API 凭据（进行中）

新增 connection credential set/clear；set 只接收标准输入管道，不支持秘密 argv。首次操作先持久化请求目标与 Keychain 引用，再在数据库事务之外访问 Keychain，最后提交引用和调用结果。重放校验同目标及同秘密；清除有独立持久意图，Keychain 删除后数据库失败可按原请求补提交。代次跨清除继续递增，旧清除请求不影响新凭据。新连接版本不自动继承旧秘密，可通过 --version 显式修复原冻结配置。

使用 security-framework 3.7.0 的 safe Rust API，Cargo.lock 固定依赖；不启用 Keychain 交互弹窗、不回退明文。数据库和请求指纹不保存秘密，错误输出不反射误传的秘密 argv。Keychain 拒绝时保持明确不可用；本地保存与模型可用性分开，连接仍 unchecked。

新增 1 个常规输入边界测试和 2 个真实 Keychain 测试：实际 CLI 从 stdin 保存、跨进程重放、不同秘密冲突、引用及代次、发布故障恢复、旧版本修复、清除故障恢复、代次不归零及工作区无明文。早期尝试由另一个测试程序读取/删除 CLI 创建的项被 Keychain 应用身份规则拒绝；现由创建者自身读取并清理，没有放宽访问控制。早期失败运行可能留有合成测试项，未批量删除无法确认归属的 Keychain 内容；当前通过的测试显式核验清理。

本次默认 90 个 Rust 测试通过，另 2 个真实 Keychain 测试通过；2 个 Docker 测试亦在 schema 13 上通过，Clippy/格式/差异检查通过。该轮数据库为 schema 13，未发行旧格式明确拒绝。当时生产服务仍 executionSupported=false；连接测试、上下文与实际进程装配/恢复、两种隔离 CLI、返工/验收/导出及 Skill 仍为必需未完成范围。凭据保存不代表 API 订阅问题已修复，也不升级任何 Story 验收。


## API 生产运行服务、上下文与进程核对

服务现可按已有持久投递启动 API 数字员工。发现顺序按消息记录，接收者与用途来自核心已保存事实；没有交接/返工决定不制造下一阶段。缺冻结配置、凭据或接入构建先 blocked，给本人及适用负责人留下通知，不创建 Run。冻结权限/任务版本/运行额度仍由原子领取再次核验。

上下文保存于独立私有目录，绑定 Task/Worker/冻结配置/用途族及不兼容职责依据；execute 与 rework 共用用途族，普通契约更新不清空历史。使用标记、凭据代次和启动意图同事务提交，失败整体回滚；可能启动后缺失原生文件不会静默新建。完整 checkpoint 仍由固定 milkie 接入校验。

API 接入进程清空宿主环境，只保留执行所需 PATH；凭据从 Keychain 读取后仅通过私有管道提供。服务监测停服、取消和撤权，等待在途核心调用结束，回收自己持有的 Child，再确认进程组无残留；execute/rework 固定候选，其余用途核对处理终局。协调和检验不创建执行候选。run show 增加 API 上下文、凭据代次、终态和接入停止观测；这些字段不是任务验收事实。

重启后先核对旧检查，再观察已登记 API 进程组是否消失；不向旧 PID 发信号，不按时间或租约释放 Run。启动后尚未来得及保存 PID、资源仍存活或原生资料/检查/产出无法核对时保持 unknown，占用名额，正式 reconcile/继续入口仍未完成。

新增 2 个核心测试覆盖绑定隔离、契约变更保留、冻结职责变更分离、重启、凭据代次及启动事务回滚、丢上下文拒绝新建。新增 1 个需显式运行的真实 CLI/Keychain/Node 测试验证独立子进程、停服、强杀服务后恢复、blocked 不冒充 handled、预算不重置与失败不自我唤起。首跑发现协调 Run 错误调用候选准备，已修正为仅 execute/rework 准备候选，后续真实测试通过。测试使用合成秘密和 example.invalid，清理自身 Keychain 项；不计真实模型成功或完整 S 验收。

另补旧版本普通消息取消、不生成失败通知，以及准备阶段遇到停服保持 queued 的测试。该轮 schema 14；常规 94 个 Rust 测试（2 Unit、16 CLI、76 核心/桥接/管道）与 17 个 TypeScript 测试通过；另行运行的 2 个真实 Docker 和 3 个真实 Keychain/CLI/Node 测试通过，Clippy、格式及差异检查通过。Docker 本轮证据为 `.agents/verify-runs/1/core-docker-check-4a326666-860e-4d24-8e2d-130956662a9c.json`。真实 API 订阅问题尚未收到修复信息，没有重试或声称恢复；连接测试、两种隔离 CLI、执行者自测、返工/恢复/验收/导出、产品 Skill 与完整真实验收仍是后续必需工作。


## 有界 API 连接检查与诊断版本

新增 `connection test <ID> --revision <N> [--version <版本>]`，必须提供全局 requestId。API 从该版本的 Keychain 引用加载秘密，经独立私有管道发送最小无工具请求；不建立 Task、Run、工作消息或原生会话。CLI 类型要求 `--worker` 并校验绑定，当前记录 agent_cli_unavailable，不冒充隔离环境可用。

请求与 pending 检查记录先原子保存，完成时检查结果和 request show 同事务更新。重复请求只返回原记录，不再次探测；结果丢失保留 pending，不能从旧进程/超时猜测成功，要重测须显式新 ID。诊断状态和业务授权分开，连接检查失败不修改团队权限，也不自动派发工作。

connection show 在同一读快照查询当前连接、凭据代次和适用检查，返回 unchecked/ready/unavailable/inconclusive 及 lastTest 的当前适用性。连接名称、Worker 工作说明变化不清空有效结果；更换模型/端点、凭据代次或接入版本使旧结果不再代表当前。旧版本检查不会遮盖当前版本已有诊断。

专用 Node 检查进程不装配工具或 AgentRuntime，输出严格限于固定状态/错误类别和可选 HTTP 状态；父进程约束输出大小并核对实际 Child 退出。检查进程 25 秒截止，父侧观察 29 秒并预留 1 秒停止回收。真实 CLI/Keychain 测试使用合成凭据与 example.invalid，已走实际接入并观察 connection_failed，验证重放、清除/换代失效、不回显秘密和无业务任务；这不是成功模型连接证据。

新增 3 个 Rust Unit 与 1 个真实 CLI 集成测试，常规共 98 个 Rust 测试通过；TypeScript 增加最小请求、错误/伪工具响应、凭据输入、真实进程断管及截止时间测试。当前 schema 15，旧未发行格式明确拒绝。真实 API 订阅未收到修复信息，本轮没有重试此前失败的真实账号；完整 Story 验收仍未通过。

本轮最终检查：98 个常规 Rust、22 个 TypeScript、另行显式运行的 2 个真实 Docker 与 3 个真实 Keychain/CLI/Node 测试通过；Clippy、格式和差异检查通过。真实无输入子进程约 25 秒按约定返回 deadline；schema 15 的 Docker 证据为 `.agents/verify-runs/1/core-docker-check-ce5da72b-e1c3-4784-96e7-2d262786ba1b.json`。全部仍为开发检查，当前工作区未冻结，S1–S7 未标为通过。


## 执行者自测与候选版本证据

执行/返工成员现可通过已有 `run_check` 运行冻结命名检查；候选清单快照、版本与摘要、检查记录及成员请求账本同事务落库。检查容器只读取该快照的不可变内容对象，候选后续修改不会替换检查输入；服务/数据库重开后仍使用保存的清单。执行成员提交产出或本轮处理结果后不能发起新的自测。结果查询明确区分 candidate 和 artifact 目标；自测通过不创建独立检验、交接或验收，也不自动选择后续阶段。

自测复用现有无网络、只读和固定资源限制的检查容器。`verification_submit` 继续要求冻结检验成员、已接受的交接及本检验 Run 最新真实检查，并明确只接受匹配交接产出的 artifact 目标；相同内容摘要不能把自测升级为独立检验。旧检查容器资源未核对时仍阻止释放 Run 或启动另一个检查。

新增核心 Integration 验证检查与快照随账本失败一起回滚、请求重放、候选修改后旧快照保持、活动检查互斥、工具范围和伪造独立检验拒绝。新增真实 Docker Integration 先保存缺陷版本的检查请求，再用成员文件工具修复候选并重开数据库：原检查仍失败，新请求通过；提交后的新自测被拒绝，已完成检查重放保持原结果，固定产出与新版自测摘要一致且没有独立检验记录。这些成员操作及修复仍由测试构造，不能替代真实模型、CLI 和产品 Skill 的完整验收。

本轮检查：99 个常规 Rust 测试（5 Unit、17 CLI、77 核心）通过；3 个真实 Docker Integration 通过，Clippy、格式和差异检查通过。自测证据：`.agents/verify-runs/1/self-test-cbb952e4-d0de-45bf-b5ad-38e1325f1004.json`；独立检验回归证据：`.agents/verify-runs/1/core-docker-check-9d89bf0e-54e3-4eb1-9225-4f037c0716f0.json`。本轮未修改 TypeScript、凭据或模型请求，未重复这些环境测试；上一轮 22 个 TypeScript 和 3 个 Keychain/CLI/Node 结果仍作为历史记录，不冒充本轮执行。schema 16 尚未发行，仍无冻结验收候选或 Story 通过。


## 有限返工：安排、固定输入与预算

新增 CLI `task rework` 与成员 `task_arrange(action=rework)`。当前支持引用同任务当前产出的失败独立检验，或最近一次已核对停止且没有业务终局的执行失败；本机本人不具备该任务团队负责人职责时仍拒绝。安排固定前次执行、原因记录、输入产出和当时当前产出；有候选但未固定 partial 时拒绝丢弃内容后重新开始。已有执行或检验安排、未知资源及过期依据不能被新请求绕过。失败检验只保存事实，没有团队负责人决定不会自动返工。

排队和未领取的 blocked 返工保留额度预留；预留、消息、请求结果在同一事务保存。领取时将预留转为已消费，并与总 Run 消费及 prepared 登记同事务；失败不退已消费额度，领取前取消释放预留，重启和重复请求不改变计数。`task show` 增加当前预留与安排记录，成员 `task_read` 提供同等事实。候选准备读取安排指定的不可变产出，不在领取后改读另一个“最新”版本；新版产出须独立重验。

三个常规 Integration 覆盖运行中拒绝、安排和领取事务故障回滚、同请求重放、重启保留额度、2 次返工上限、旧运行原因拒绝、撤权后 blocked 保留预留、取消释放、成功执行不能伪装失败，以及数字团队负责人实际成员调用安排/记录消息终局、本机本人不得代办。一个新增真实 Docker Integration 使用真实 CLI 安排返工，成员工具修复候选、提交新版并再次独立检查；旧失败记录保留，旧检查不能提交新版结论。上下文用途族关联相同已有检查，但本测试不声称验证了真实模型原生续接。

阻塞报告/解决及验收拒绝的正式记录尚未实现，相应返工原因仍是必需后续工作；未提供伪造理由绕过。正式恢复、两种隔离 agent CLI、产品 Skill、验收/导出和完整真实成员路径仍未完成。以上是开发集成证据，S3/S5/S7 等全部 Story 继续未验收，未冻结、未独立审查、未创建 PR 或合入。

本轮检查：102 个常规 Rust 测试（5 Unit、17 CLI、80 核心）通过；另行运行 4 个真实 Docker Integration 通过，Clippy、格式及差异检查通过。新增 CLI 参数检查覆盖返工原因缺失和互斥冲突，均单 JSON 错误返回。返工证据：`.agents/verify-runs/1/rework-85232916-429e-4c6d-870d-7ee107561f71.json`；独立检查回归证据：`.agents/verify-runs/1/core-docker-check-2479d810-32f9-46e4-bc53-e676dc779eb8.json`。本轮未改 TypeScript 或凭据实现，未重复旧 Keychain/TS 测试，也未重试尚未修复订阅的真实模型 API。当前 schema 17；未发行旧格式继续拒绝，全部 Story 仍未验收。


## 正式阻塞、停止核对与解决后的继续

新增成员 `task_report_blocker`：保存 Task/Run/投递、处理者、原因和 open 状态，向处理者/团队负责人保存必要通知；数字团队负责人报告自身阻塞时通知本人，避免自我唤起。报告、通知、成员处理结果、停止请求与工具账本同事务；报告成功不释放 Run。停止请求后旧成员通道拒绝继续调用，已保存事实可通过 `task blocker show/list` 查询，不因丢失工具回包而重复报告。

新增 `task blocker resolve` 与指定获准协调成员的 `blocker_resolve`，校验当前任务版本、阻塞版本、身份与权限，要求源 Run 和其它相关执行资源已核对停止；处理者自己的协调 Run 可以记录解决依据。解决事务保存原契约范围内的依据并通知团队负责人，不自动重排或运行。当前版本仍有 open 阻塞时禁止新执行/检验；取消或 pending 版本更新会保留 superseded 历史并终止旧通知。

执行阻塞解决后，团队负责人以 `rework --blocker` 或成员 reason.kind=blocker 明确继续，仍消费返工额度；非空候选必须先固定 partial。对于受控清单为空的阻塞执行，生产 API 完成路径在所有子进程与在途操作停止后可以不生成 Artifact 而结束，未知资源核对前不能解决。检验阻塞解决后用 `verify --blocker` / task_arrange 的 blockerId 明确重新交接同一产出；旧交接保存为 superseded，新交接和新投递原子产生，不沿用旧接收决定，也不消耗代码返工额度。

新增 Integration 覆盖报告/账本及解决通知故障回滚、停止前/unknown 拒绝解决、无权处理者拒绝、无产出→真实 CLI 解决→返工、已接收与未接收检验的重新交接及事务回滚、取消后迟到解决拒绝，以及数字团队负责人在协调 Run 中解决并另行安排返工。新增真实 Node 管道测试让子进程报告阻塞、接收核心停止信号后退出；投递仍须回收子进程并可信核对后才 handled。测试成员与环境修复依据均为 fixture，不作为 S3/S5/S7 的真实团队验收。

正式恢复待办/投递 retry/未知资源核对管理入口、验收及拒绝后的继续、产出导出、两种隔离 agent CLI 和产品 Skill 仍是必需工作。未修改真实 API 订阅配置，没有把环境未修复作为其它缺项的替代解释。当前工作区仍未冻结、未提交或送独立审查；全部 Story 保持未验收。

本轮检查：108 个常规 Rust 测试（5 Unit、17 CLI、86 核心/管道）通过；4 个真实 Docker Integration、Clippy、格式及差异检查通过。核心中新增 6 个阻塞相关 Integration，含真实 CLI 解决/返工和真实 Node 停止管道；最后的接口整理另聚焦重跑检验阻塞重新交接。Docker 回归证据：`.agents/verify-runs/1/core-docker-check-17f53074-ecf0-4d87-93eb-4f1c6f0ac79d.json`。本轮没有改 TypeScript 生产源码/凭据，也未请求真实模型，既有 TypeScript/Keychain 结果只作历史记录。当前 schema 18；未发行旧格式 1–17 拒绝，不覆盖旧目录。

重新交接同时保留旧交接的原状态、接收理由和所引用 blocker，不能用替代原因覆盖原接收历史；对应 Integration 已断言。后续检验重试还须覆盖已有 inconclusive 终局后的新交接，不能重放已提交终局；当前本轮只完成正式阻塞解决后的重新交接。

## 人工验收、拒绝后新版继续与产出导出

新增 `task acceptance request/show`、`task accept/reject`、数字团队负责人 `acceptance_request`。请求绑定 Task revision、完整当前产出和其最新独立通过检验；核心同时核对实际检查、摘要、配置、冻结检验者与执行者不同、相关资源已停止、无未解决阻塞和待生效执行安排。请求允许负责人自己的协调 Run 尚在运行，提交接受/拒绝则必须等待该 Run 核对停止。数字成员没有接受/拒绝工具，本人也不能替数字团队负责人提出请求。

接受事务保存不可变决定、请求 accepted、本人投递 handled、Task closed/accepted，并使其它未完成待办过期。拒绝事务保存不可变决定、请求 rejected、本人投递 handled、给团队负责人的结果消息，Task 保持 active。两种决定要求当前及冻结验收/通信授权；普通说明、通用 decision respond/record、数字成员工具或换请求 ID 都不能替代。拒绝的产出不能重新请求，负责人须通过 `rework --rejection` / reason.kind=rejection 另行安排新版，再独立检验和创建新请求；拒绝通知只能以对应返工安排或明确等待结果处理。旧拒绝保留，不阻塞新版。

新增 `artifact export`：只读指定产出及清单内容，支持当前、历史和 partial，返回摘要、文件清单和限制。先核对所有对象，再写入同父目录的独占临时副本并通过目录重命名发布；拒绝非空/链接目标和工作区内部目标，父目录须已存在。只导出清单文件，不复制数据库、凭据或上下文，不触发验收/运行/发布。它不使用 Task 请求账本；再次向非空目录导出明确拒绝，可用另一空目录。强杀可能留下未发布临时目录，不能当作成功交付。

新增 7 个常规 Integration：验收停止前拒绝与事务回滚/重放、拒绝通知回滚及同产出重开拒绝、当前权限与取消失效、独立证据/版本门禁、数字负责人通过 mailbox 请求验收并处理拒绝、partial 实际 CLI 导出和无覆盖、损坏对象/清单导出拒绝。既有真实 Docker 返工测试扩展为失败与人类拒绝两条场景，都使用真实 CLI 请求/决定/返工/导出并重新独立检查，新版接受才关闭；历史与当前导出内容逐一对应。检查、CLI 和数据库为真实组件，模型操作与人类决定为 fixture，未记任何 Story 验收通过。

本轮检查：115 个常规 Rust 测试（5 Unit、17 CLI、93 核心/管道）、5 个真实 Docker Integration、Clippy、格式及差异检查通过。本轮未改 TypeScript 或凭据实现，不重复历史 Keychain/TS 测试，未重试未修复的真实 API 订阅。数据库 schema 19，未发行旧格式 1–18 明确拒绝、不覆盖。工作区未冻结或提交、未独立审查、无 PR 或合入。

剩余必需工作仍包括正式恢复待办/投递 retry/未知资源核对管理入口、已有 inconclusive 终局后的新交接、两种隔离 agent CLI 及登录/检查、产品 Atelier Skill、真实 API 成功与全部 CLI/Skill 真实团队路径；S4 的真实 Skill 决定和实际游玩证据仍未取得。环境问题不替代其它实现责任，全部 58 项保持未验收。

本轮 Docker 回归原始证据：`.agents/verify-runs/1/core-docker-check-6d932ccd-c4a0-4b76-85f3-f66a2f12acbc.json`；两条返工/验收路径：`.agents/verify-runs/1/rework-4c4cdd8c-b7d7-46a3-b6d0-edb9417c06b4.json`、`.agents/verify-runs/1/rework-b0c93c25-8ac5-471c-98c4-b73f5cc6f64b.json`。

## 不确定检验终局后的明确重新交接

`task verify --inconclusive <Verification ID>` 和成员 `task_arrange(action=verify, verificationId=...)` 已实现；与已解决阻塞引用互斥。只有冻结团队负责人按当前授权，引用同契约、当前产出最新的 inconclusive 检验，且原检验与其它相关资源已停止，才能新建交接；负责人自己的协调 Run 可记录安排后结束。明确 pass/fail、混用原因、重复或过期依据均拒绝。旧交接替代、新投递和请求/成员操作账本同事务，失败整体回滚。

旧交接保留原接收状态、理由与引用的不确定检验，旧检验记录及已处理投递不重放。新交接须重新接收，消费总 Run 额度而不增加代码返工；新检验只能使用本 Run 的新检查，不能引用旧证据。核心没有根据 inconclusive 自动重新排队或运行。

新增 3 个常规 Integration 覆盖真实 CLI 重交接、事务回滚、未停止拒绝、旧证据拒绝、旧历史保存、明确 pass/fail 与冲突原因拒绝，以及数字负责人从结果消息明确安排和处理终局。新增 1 个真实 Docker/CLI Integration 对相同固定产出运行两次独立检查；第一次检查通过但 fixture 成员给出 inconclusive，第二次新交接/新检查后给出 pass 并正式接受。它证明组件和约束接线，不声称发生了真实检查故障或真实模型决定。最初测试夹具复用了已有 `accept` 请求 ID，核心正确拒绝，改为独立 ID 后完整路径通过；未放宽去重。

开发检查：118 个常规 Rust 测试（5 Unit、17 CLI、96 核心）通过；新增真实 Docker 场景通过，完整 Docker 回归结果与证据见本节后续记录；Clippy 通过。追加的 CLI 参数互斥断言已聚焦通过。schema 保持 19：交接新增可选历史引用，无 SQL 结构变更。全部 Story 仍未验收，未冻结/提交/独立审查/合入。新场景证据：`.agents/verify-runs/1/recheck-ab544b36-8208-4e1d-9e57-266747f2f01a.json`。

后续正式恢复还需同时处理配置前置失败与已停止的协调/检验 Run；成员自己更新契约或承接后，原消息版本保持历史值，但 Run 已记录当前有效版本，恢复不能因此丢弃同投递内已提交操作。实现恢复须沿用持久调用账本与原生上下文，先核对未获回复调用，再允许新的模型操作；无终局的执行/返工不得原样 retry，须另建有原因引用的返工。当前尚无 mailbox retry 或正式恢复待办入口，不能把本节重检入口当作通用恢复完成。

完整 Docker 回归 6 项全部通过；原始检查证据 `.agents/verify-runs/1/core-docker-check-16635694-53ac-41ab-a2d9-b88e24825e0d.json`，本次新交接/新证据路径 `.agents/verify-runs/1/recheck-f2c909cd-e4de-4e7f-9a42-df48caa0c9bb.json`。Issue 进度已更新并读回核对，Stories、范围和全部未勾选验收标准未变。

## 显式投递重试与同投递继续

新增 CLI `mailbox retry` 与数字团队负责人受控 `mailbox_retry`，要求投递版本和原因。本人须当前及冻结管理/通信权，团队负责人须安排/通信权；接收成员也重新核验冻结配置与当前/冻结权限。未登记 Run 的前置失败可重新排队；已停止但无终局的 coordinate/verify 保留原投递、操作账本和历史 Run，再次领取消费新的总 Run 额度。queued 只代表等待服务前置核对，不声称凭据/原生上下文/资源可用。已受理 execute/rework 必须由负责人以正式原因另建返工；重试不能解除 unknown、重置预算或重跑终局。

成员补齐契约或承接已提交后，原消息保留历史版本，原 Run 保存推进后的有效版本。重试、服务发现与领取共用该投递最后已停止协调/检验 Run 的版本依据，并要求等于当前 Task；其它外部版本变化仍拒绝。旧操作以 originatingRunId/operationId 返回原持久结果，不重复承接或发送消息；新的决定由新 Run 作出。`task_read` 增加本任务范围内投递索引，原因预览限制 256 字符并标记截断，CLI mailbox 返回原消息任务版本。`message_respond(kind=retry)` 只接受本投递期间真实重新排队的结果，已有普通安排不能冒充。

五个新增 Integration 覆盖：真实 CLI 承接后继续且旧操作不重放、请求账本失败与投递回滚；unknown 与额度耗尽拒绝、已受理代码执行拒绝；终局只查询、未运行重试不扣额度、重复请求；数字负责人真实成员工具重试与处理终局、工具 schema；撤权/恢复与旧任务版本校验。运行资源、模型决定和前置失败仍是 fixture，不证明真实模型/Skill 恢复完整路径。123 个常规 Rust 测试（5 Unit、17 CLI、101 核心）、Clippy 和格式检查通过；本轮未改 Docker/Keychain/TypeScript 接入实现，未重跑其历史检查，也未重试真实 API 订阅。

schema 19 不变，复用现有投递/Run/调用账本；没有重置旧记录。正式恢复待办尚未实现，负责人失效仍只有失败通知；管理侧未知资源核对、两种隔离 CLI、产品 Skill、真实模型与全 58 项真实入口验收继续必需。当前未冻结/提交/独立审查/合入，全部 Story 保持未验收。


## 本人恢复待办与离线资源核对

负责人前置失败、撤权、额度不足或 Run 失败时，核心同事务保存 kind=recovery 待决定事项与本人通知；按原投递及事件去重，存在未完成事项时合并通知，不产生负责人自我唤起，也不受业务消息额度耗尽影响。本人使用 decision respond 保存 retry/wait/cancel，可在 responded 按新版本更改选择；旧回应投递保留并取消。recovery apply 校验固定本人当前与冻结管理/通信权，实际调用重评估或取消；业务拒绝回滚效果并持久 blocked_reason，存储失败连同请求账本整体回滚。等待不启动，回应不授权，取消仍须等待未知资源结束。

mailbox retry 尊重本人恢复选择；已保存终局只核对并闭合选择重试的待办，不重跑模型。无终局重新排队后事项 resolved 仅代表恢复操作已提交，不代表环境或新 Run 成功。新 Run 再失败产生新事件事项，历史操作和决定保留。

新增 runtime reconcile：离线独占服务锁，服务在线明确拒绝；新 epoch 下请求停止并复用可信资源检查，完成数据库桥接排空。检查容器须核对归属；API 进程组须实际消失，缺 PID 登记继续 unknown；不按旧 PID 杀进程。结果及状态 stopped/blocked_unknown 持久保存，返回未核对 Run 列表。核对从不领取排队消息或消费 Run 额度，也不替团队负责人安排业务阶段。

本轮新增 6 个常规 Integration：本人等待/重试/取消、响应与请求账本回滚、撤权和预算耗尽通知、已存终局的待办闭合、真实进程组存活/退出和缺 PID、服务锁拒绝及排队保持。另更新真实 CLI/Keychain/Node 场景，在强杀实际服务后使用 runtime reconcile 核对旧接入退出；该场景通过，使用合成凭据与无效端点，不是真实模型业务成功。数据库 schema 20 增加恢复事件唯一约束及 blocked_unknown 控制状态；未发行旧格式明确拒绝。

两种隔离 agent CLI、产品 Atelier Skill、真实 API 成功与全部 CLI/Skill 团队路径仍需完成；模型订阅尚未收到修复信息，本轮未重试。工作区尚未冻结/提交/独立审查，无 PR 或合入，全部 58 项仍未验收。当前 L1 v0.10 沿用既有资源核对与恢复规则，只补齐管理命令入口；L2 同步具体状态与接口，不削减范围。


本轮最终检查：129 个常规 Rust 测试（5 Unit、17 CLI、107 核心）、Clippy、格式及差异检查通过；另外显式运行 1 个真实 Keychain/CLI/Node 崩溃核对场景和 1 个真实 Docker/CLI 资源归属场景均通过。后者核对同名但归属不符的容器必须保留、确属当前检查的容器才清理、其它 Run 资源仍 unknown；重复核对没有新检查或 Run。未修改 TypeScript 生产实现，本轮未重跑其历史测试，也未重跑另外 5 个 Docker/2 个 Keychain 场景；历史结果不冒充本轮全量验证。


## 产品 Atelier Skill 与核心选择的指导

提供 skills/atelier 产品源包，宿主安装只复制最小 SKILL.md，成员共同概念与协调/执行/检验资料编入 CLI。skill install --destination 要求新目录或完全相同入口；修改内容、链接目标不覆盖，不创建工作区、不启动服务。skill describe --protocol 2 区分空工作区与损坏/不兼容格式；--team/--task 互斥，从同一读快照取得本人、当前授权、冻结任务职责和版本，返回适用操作索引。没有 actor/role 自选入口，管理者不冒充数字负责人，未选择范围不推断团队权限。响应与落实指导按实际指定处理者/发起者选择。

成员描述使用现有工具登记生成 operations 文件、schema 和根索引，选择对应职责资料；重复/缺失登记拒绝装配。当前权限与 Run 保存权限取交集，不把新增权限热注入旧 Run；API 使用等价内联说明，隔离 CLI 文件包接入仍需完成。doctor 已区分 Skill 入口支持与完整团队可用性；说明不代替核心授权，安装不等于真实宿主已加载或验收。

新增 4 个 Integration 覆盖实际 CLI 安装重放/不覆盖/链接拒绝、空/损坏工作区和协议错误、当前权限与冻结负责人选择、成员工具/schema/职责文档一致且无越权路径。常规 133 个 Rust（5 Unit、19 CLI、109 核心）通过；Clippy 通过。源包通过 skill-creator quick_validate，验证器 Python 环境缺 PyYAML 后使用 uv 隔离依赖运行，未修改全局 Python。

当前 Codex 宿主实际安装并读取最小入口，按 describe 指引初始化专用工作区、创建 4 Worker/1 Team、显式授权并保存 pending Task/queued 投递。服务未启动、Run 0，本人操作索引没有数字负责人协调入口。原始 JSON 与 host-observation.md 位于 `.agents/verify-runs/1/skill-host-ddb804b6-990e-4e14-8460-f6c1f2ce9213/`；临时复制的 Skill 已清理，未修改用户全局 Skill。它是开发观察，不是独立前向测试或冻结候选验收；未取得具名模型完整链路、真实人类决定或宿主退出推进证据，全部 Story 仍未验收。

本轮全量回归暴露服务状态读取竞态：先读数据库再查锁可能把已正常停止的服务误报 interrupted。改为先尝试持有共享锁再读取快照，服务释放独占锁后可见其最终事务；没有放宽原测试断言。并发 start/stop 回归重复执行结果见本节最终记录。两种隔离 agent CLI、真实 API 成功及全部真实团队路径仍需完成，尚未冻结/提交/独立审查、PR 或合入；schema 保持 20。


本轮最终检查：133 个常规 Rust、Clippy、格式和差异检查通过；已失败的并发启动/停止场景在修复后连续 10 次通过。新增指导装配后另显式运行 1 个真实 Keychain/CLI/Node 启停与强杀核对场景通过，证明生产接入仍可接受本轮装配；使用合成凭据和无效端点，不是真实模型成功。TypeScript 源码、容器及凭据实现本轮未改，未重复其它历史 Docker/Keychain/TypeScript 检查；当前 Skill 的真实模型效果仍须成功业务验证。Issue 同步只记录进度，未勾选任何验收项。


## CLI 出站隔离组件与真实网络检查

重新核对 Atelier 和相邻 milkie 的 origin 均为 GitHub；milkie 远端 main 与本地 HEAD 仍是 7865ffcc14a8359a055e5e6e0998b56ab2160379，#263 OPEN，本地执行 SDK 为未提交修改。草稿选择 Grok/Pi，只支持 standard/read-only 原生工具模式，缺 Atelier 所需受控工具回调及指定隔离启动入口。未修改或打包相邻工作区；已向用户询问是否由本任务补齐依赖，在答复前继续 Atelier 侧独立工作。

新增可信出站代理：精确域名/443 CONNECT、公开 IPv4 答案逐项校验及数值地址固定连接、私有启动配置、连接/时间/输入上限和控制管道关闭退出。不记录正文、头或凭据；代理不解密 TLS，不替代真实 CLI 的工具约束。生产运行服务尚未装配该组件，不宣称 CLI 可用。

实测 Docker 27.4 不支持 isolated gateway 模式。改用有官方文档支持的 internal + inhibit_ipv4，实际核对宿主桥没有内部网关地址；并非只看配置标志。新增 live 场景创建自己的宿主 TCP 服务，普通 bridge 访问成功作为对照，内部容器访问网关/其它宿主地址/公网/外部 DNS 均被阻断；双网络代理只监听内部地址，批准 example.com:443 取得真实 HTTPS 200，未批准域名和回环目标返回拒绝，控制管道关闭后容器停止。所有容器/网络带唯一标签且已清理。

开发检查：25 个 TypeScript 测试通过（新增 3 个代理策略/真实 TCP/配置脱敏），另显式运行真实 Docker 隔离场景通过。第一次 live 检查的 8 秒观察窗口遇到 HTTPS 超时，同站点普通 bridge 对照随后正常；将 live 观察窗口改为有界 18 秒并保留全部断言后，两次完整检查通过，生产代理时间约束未放宽。最终原始证据 `.agents/verify-runs/1/isolation-b48f05e3-c48a-4eff-8853-604d67674129.json` 包含实际代理模块摘要、镜像与 cliExecution=false。Rust 实现本轮未改，133 项 Rust 与 Clippy 是上一轮结果，没有重复计为本轮执行；格式/差异检查通过。

下一步仍须接入两种真实 CLI 的镜像准备、Worker 专用登录、受控工具、隔离执行与原生续接，并完成 API/CLI/Skill 的全部真实路径；网络组件检查不能替代这些要求。当前仍为未提交工作区，无冻结候选、独立审查、PR 或合入，58 项验收保持未通过；schema 20 与固定 milkie 依赖未变。


## CLI 隔离资源登记与崩溃核对

新增核心 cli_resources 记录，在实际 Docker 创建之前与 Run 启动意图同事务保存固定镜像、Docker 引擎身份、归属标签与四个资源名称。核心拒绝 API 连接、旧 epoch、已启动 Run、过期任务及撤权；保存失败整体回滚。停止和产出发布共用资源门禁，尚未核对不能释放工作区名额。数据库 schema 21，未发行旧格式 1–20 明确拒绝而不覆盖；未新增产品权限或模型工具。

运行服务和离线 runtime reconcile 已接入回收：先证明原接入进程组不存在，再核对同一引擎，按不可变对象 ID 删除归属相符的执行容器/代理，最后删除空网络。缺 PID、活进程、异主同名资源、仍有挂接容器的网络或 Docker 故障保留 unknown；不按旧 PID 杀进程，不强制断开外来容器，不删除登录或会话卷。run show 返回 cliResources 及原因。该登记/回收组件尚未被真实 agent CLI 启动路径调用；两种 CLI 接入能力缺口依然存在，不把恢复夹具当作已实现 CLI 执行。

新增 2 个常规 Integration 验证登记事务回滚、冻结配置/epoch、停止门禁、持久查询与缺 PID；新增 1 个显式真实 Docker Integration 验证存活进程不清理、引擎不一致拒绝、异主容器保留、外来挂接不强断、清理成功及反复核对。测试中的执行器和代理容器是合成 Node 进程，没有真实 Grok/Pi 或模型调用。最终证据 `.agents/verify-runs/1/cli-recovery-5d0a5a21-6912-486d-8b58-1cff1d4d268f.json`，cliExecution=false；测试所属容器/网络已清理。

135 个常规 Rust 测试（5 Unit、19 CLI、111 核心）通过，新增真实 Docker 检查通过；加入引擎身份核对后重跑受影响常规/真实 Docker 场景，Clippy、格式和差异检查通过。TypeScript 生产实现未再改动，25 项及出站网络 live 的结果为上一轮证据，本轮未重跑。当前仍在 implementation，全部 58 项未验收；没有冻结候选、独立审查、PR 或合入。

重新核对 milkie #263：远端 main 仍为 7865ffcc14a8359a055e5e6e0998b56ab2160379，CLI 分支未发布；本地草稿缺受控工具回调/指定隔离启动。没有修改或打包邻仓未提交工作。继续需要可固定的上游接入以及可用真实模型连接；此前提出的依赖工作范围问题尚未得到明确答复。

本轮 Issue 进度已同步并读回逐字核对；Stories、范围、验收标准保持原文，未勾选任何验收项。


## 上游 CLI 能力探测与依赖边界澄清

重新读取 milkie #263 的实际范围：它承诺统一执行/查询/取消、原生续接及 read-only/standard 工具策略，未承诺 Atelier 的受控自定义工具回调。该票当前 OPEN，远端 main 仍为固定基线，CLI 分支没有可引用提交；因此不能仅等待该票关闭就认定 Atelier 的接入要求满足。未改写相邻 milkie 工作区，也未向其它任务发送消息。

纠正此前把“指定隔离启动 API”也列为上游必需扩展的说法：Atelier 可以在受控容器内启动整个 TypeScript 接入与 milkie SDK，由既有容器资源管理负责隔离与回收，SDK 的 Linux 支持及实际信号/存储行为仍须验证。必须补齐的上游能力是按 Run 装配自定义工具/回调、严格关闭未获准原生工具、稳定关联调用结果/取消及续接；登录与原生配置仍需 SDK 吸收，不在 Atelier 私造各 CLI 命令。

本机实际版本为 Pi 0.85.1、Grok 1.0.41 (4220f3b224a6)。使用新的专用 HOME/配置目录，没有沿用宿主登录。Pi 在关闭扩展发现、Skill、模板、主题与上下文文件发现后，仍能显式加载测试扩展；关闭内置工具且精确工具白名单时，实际启动只启用 atelier_probe。第一版无模型空闲探测未落盘会话，两次启动会话 ID 不同，原续接断言失败；已保存失败观察，没有将它标为续接成功。

随后使用本地确定性 SSE 模型夹具驱动真实 Pi CLI：模型请求的工具集合仅 atelier_probe；原生 toolCallId=probe-call-1 到达扩展回调一次；回调结果保存；CLI 退出重启后使用同一原生会话，后续模型请求含历史工具结果。模型响应是 fixture，无真实提供商调用，不能替代 S2.A6 或 S3.A5。Grok 在专用配置中注册合成 stdio MCP 服务，实际 doctor 成功握手并发现唯一工具；尚未证明 Grok 模型调用、自定义工具权限过滤或原生续接。

原始证据目录 `.agents/verify-runs/1/cli-capability-a06229e2-2673-4566-98bc-b9199041fd86/`：pi-result.json 保留首版限制；pi-controlled-tools-result.json、两轮 JSON 事件、原生合成会话和回调记录保存 Pi 探测；grok-doctor.json、mcp-calls.jsonl、grok-result.json 保存 Grok 探测。所有结果均 productAcceptance=false。临时模型服务和所启动子进程均已退出；未修改全局登录或配置。未改生产代码，未重复 Rust/TypeScript 套件；完整真实 CLI、登录、真实 API、全部验收与审查发布仍未完成。上游增补范围仍待用户决定。

当前已将上述实现、设计和功能地图保存到工作分支开发检查点；历史段落中的“未提交”描述仅对应当时阶段。尚未完成全部实现/验收，不创建 PR、不合入默认分支。


## 2026-10-03：消费 milkie 已合入的 CLI 接口（开发检查点）

L1: reuse v0.10，全部 58 项验收保持；L2: write v0.11，仅补充上游 SDK、完整 schema 校验和 CLI 回复恢复契约。依据是此前用户确认的范围及本次继续端到端完成授权。当前阶段仍为 implementation，没有冻结候选或产品验收完成表。

- milkie 固定依赖从 7865ffc 升到合入提交 e049f0b12479b07456e9c10acd709579ca3cd47f；用独立 Git archive 构建，邻仓未提交实验代码未进入包。Rust/TypeScript 握手和 NPM lock 同步。
- CLI 工具定义保留完整 schema，只给 enum/const 补充确定的类型；固定 Ajv 8.17.1 在宿主回调前校验枚举、分支、长度、范围、未知字段，不作参数转换。业务权限仍由核心执行。
- CLI 单次执行通过真实 ExecutionClient 注册串行工具；持久关联 SDK Run 和 Atelier Run/Task/投递，用 milkie callId 关联原操作，Pi 原生 ID 不作为业务去重键。
- 恢复时重查当前授权及原效果；已排队但未交给核心的调用只核对为未执行。丢回复后停止 CLI；核对结果持久保存并进入下轮输入。旧资源未停止或投递不匹配均拒绝开始新模型执行。
- 生产私有进程入口新增 agent-cli 分支；专用上下文 manifest 绑定配置与身份，原生上下文跨投递保留，账本按投递分目录。成功握手不声称容器隔离已验证。
- 聚焦开发测试已验证完整 schema、真实 SDK/监督进程/Unix socket、丢回复恢复、未派发调用、恢复后撤权、投递不匹配、私有进程跨投递续接。CLI 响应为明确的 Pi 协议夹具，不含模型调用，不替代真实 CLI 或 Story 验收。

剩余：Rust CLI 驱动与容器装配、显式专用登录/能力检查、跨投递旧 pending 的可信核对、实际 Pi/Grok/模型 API 与两条真实团队入口验收，以及独立审查、CI、PR 和合入。上游 #270 容器心跳问题继续保留待复测；不因上游功能合入而勾选 Atelier 必需验收。


本次开发检查：`npm --prefix adapters/milkie test` 为 36 passed（新增 CLI 11 项）；`cargo test --locked` 为 5 Unit + 19 CLI + 111 core passed，10 项显式 Docker/Keychain 测试 ignored，本轮未重跑；`git diff --check` 通过。上述为开发回归，不是冻结 SHA 上的 L1.8 产品验收。

后续还需覆盖 API 原生上下文接收不同投递的路径：现有 API 共用上下文账本目录，而记录按 Delivery 身份严格绑定；不能让不同投递读取彼此账本，也不能用忽略旧文件的方式跳过未核对操作。CLI 已按投递分目录，API 接线及旧开发上下文的明确兼容处理需一起核对。

## 2026-10-03：隔离启动器、冻结出站策略与跨投递账本

L1: reuse v0.10，58 项验收不变；L2: write v0.12，补充当前实现所需的冻结策略、原子启动和账本兼容契约。仍处 implementation，没有冻结候选、产品验收完成表、独立审查或 PR。

- 核心新增 CLI 上下文布局检查；上下文 used、Run 启动意图与容器资源归属同事务提交。事务失败不消费上下文；提交后缺会话目录明确阻塞，不补建空历史。
- agent-cli 连接支持显式 `egress_hosts`，拒绝通配符、IP、URL、重复和不规范域名。策略随连接版本固定并写入 Run 资源记录；当前连接更新不会扩展旧 Run。旧连接省略字段时保留原序列化身份，但生产启动入口拒绝缺策略的冻结配置。
- 新可信宿主启动器从私有管道接收登记后的资源参数，创建内部网络、出站网络、双网络代理和执行容器；非 root、只读根、固定资源上限、精确私有挂载，Skill 只读。可选进程标记须与 Run 一致。启动器不代替核心资源清理，Rust 实际调度接线仍未完成。
- 新 Linux arm64 镜像准备脚本只打包显式应用文件、固定 milkie 与用户指定的 Grok Linux 二进制；固定 Node 基础镜像和 Pi 0.85.1，校验 Grok 文件摘要。已构建并验证版本为 Pi 0.85.1 / Grok 1.0.46；没有导入宿主或邻仓的登录、会话。此脚本是开发入口，不能替代产品 connection prepare/login。
- API/CLI 都通过按投递划分的操作账本进入执行。旧开发布局采用保留原身份的可重入迁移，不覆盖冲突记录；外来 pending 先阻塞。当前投递的 completed 记录也在新模型调用前重查，防止核对完成至新输入之间崩溃丢失结果。
- 新增实际 SQLite/milkie API 上下文连续两投递测试，第二轮保留前轮输入和最终回答，仅执行新投递操作。原生 turn-end 按上游契约不保留工具草稿；测试另外核对原工具结果仍在持久账本，没有将模型历史当操作事实。其他测试覆盖迁移中断、冲突、外来 pending、恢复重查、冻结网络策略和原子回滚。

当前开发回归：43 个 TypeScript 测试通过；Rust 5 Unit + 19 CLI + 113 core = 137 项通过，10 项显式环境测试仍 ignored；Clippy、格式与差异检查通过。模型响应仍为明确夹具，不是 Story 验收。

真实 Docker 集成已先通过一次，证据 `cli-container-a7d61fad-6f11-4099-8887-b5fa842c97f8.json` 位于 `.agents/verify-runs/1/`，使用当时镜像与 Pi 协议夹具；覆盖私有管道、实际 SDK 工具转发、资源上限及只读 Skill 挂载，测试资源已按归属清理。本轮后续修复后重建镜像的结果另记，不把该历史镜像视为最新代码验证。

剩余主路径：Rust 实际 CLI 调度与停止回收、显式 Worker 专用准备/登录/能力检查、旧投递 pending 的可信核对、真实 API/Pi/Grok 与 CLI/Skill 团队验收；之后才进入独立审查、CI、PR 和合入。没有把 milkie 已合入、镜像能启动或局部集成通过视为端到端完成。

后续本轮结果：最新生产镜像 `sha256:10f715b28bb1362e1844e88c25b79719382e45d85874bb2037c4153192b610dd` 构建成功；由它派生的夹具镜像 `sha256:767be44170cfa4f8fa296ae08ab0731c405bd48629c946252cbed45ef0183593` 通过真实 Docker 启动器测试，证据为 `.agents/verify-runs/1/cli-container-7b2680e7-f30c-4a78-899d-5a0dddeeac79.json`。`nativeCli=false`、`model=false`；临时容器与网络按测试归属清理。此处不重复计入 43 个常规 TypeScript 测试，也不将前轮隔离网络证明冒充本轮重新执行。

## 2026-10-03：Worker 专用 CLI 准备入口

L1: reuse v0.10；L2: write v0.13，补充既定 prepare/login 路径中的准备契约。上一轮为有提交与实测证据的 progress，本轮继续同一 Issue，不改变全部 58 项验收及最终交付目标。

新增真实 `connection prepare <id> --revision <n> --worker <id> [--version <冻结版本>]`：持久保留 preparing/prepared/failed 和固定错误类别，按 requestId 去重。先原子登记，再核对本地固定镜像、milkie/适配标签及 Docker 引擎，最后建立与 Worker/执行配置绑定的私有登录目录。connection show 返回不同成员及历史连接版本的准备事实；request show 返回与原命令一致的结果。成功仅表示 prepared_not_authenticated，loginGeneration 仍为 0，没有账号或模型成功声明。

专用目录不共享宿主 HOME，不读取已有真实凭据；拒绝链接、未归属登录内容、绑定冲突和已准备目录丢失。显式旧版本修复只允许该成员已有的冻结执行配置；准备不创建 Task/Run，活动或未知 Run 会阻止重新准备。请求与状态保存失败整体回滚；preparing 中断可按原 ID 重开继续，已完成失败须修复后新 ID 重测，不在缓存查询时重复 I/O。环境锁已提供，登录和实际 CLI 运行仍须接入同一互斥边界。

数据库 schema 22 增加 cli_environments；未发行的旧 schema 明确拒绝而不覆盖。产品 Skill 配置指导、S1 功能地图和 L2 已同步；镜像构建仍为现有显式脚本，本命令暂只核对已准备镜像，不能把该子路径视为完整 S1.A16 通过。

开发检查：Rust 8 Unit + 19 CLI + 113 core = 140 项常规测试通过，11 项显式环境测试默认 ignored；Clippy、格式与差异检查通过。另显式执行 1 个真实 CLI/Docker 镜像准备场景通过，覆盖两个成员独立目录、命令与 request show 重放、目录缺失拒绝及无业务工作副作用，证据 `.agents/verify-runs/1/cli-prepare-223d6651-dcb0-4cf1-865a-cf25b851be25.json`。该场景不启动原生 CLI、不登录、不调用模型；TypeScript 未修改，43 项为上一轮证据，本轮未重复运行。

下一主路径是专用登录（含用户交互、持久资源归属和中断核对）、按上下文使用专用登录材料、运行服务真实 CLI 调度/停止回收及能力检查。随后仍须 API/Pi/Grok、CLI/产品 Skill 的全部真实团队路径、独立审查、CI、PR 与合入。本轮没有创建 PR、冻结候选或勾选 Story 验收。

## 2026-10-03：专用 CLI 登录管理入口

L1: reuse v0.10，全部 58 项验收不变；L2: write v0.14，明确交互登录、登录代次和中断核对契约。仍处 implementation，完整团队未验收。

新增 `connection login`：新请求要求真实交互终端和已准备的 Worker/冻结连接环境；原子保存独立登录操作并使旧材料就绪标记失效。核心先登记资源创建许可及进程归属，私有 Node 进程创建限定挂载/网络的登录容器，再由前台 Docker 连接用户终端。模型不接收登录输入，持久请求不含终端正文，不创建 Task/Run。结束后核对并回收所属资源；成功只观察私有材料文件存在，记录 login_material_saved_unchecked，不声明账号或模型可用。

原 requestId 仅返回原结果或核对中断，不重新登录；失败重新尝试需要新 ID。运行服务与离线 reconcile 均处理未停止登录，仍持环境锁的活跃客户端不被接管。schema 23 增加 cli_logins，旧开发 schema 明确拒绝，不覆盖用户目录。

真实终端测试暴露并修复了生命周期缺陷：Tokio Child::wait 自动关闭保存于 Child 的 stdin，原实现观察创建进程结束时提前关闭控制管道；现在单独持有管道，附着结束后才关闭。macOS 测试用 Python 标准库创建 PTY，避免 Node socket 输入无法供 script 使用；产品入口仍直接继承用户终端。

开发检查：原有 Rust 140 项通过，新增 2 项事务/中断重放测试通过；11 项环境测试默认 ignored。TypeScript 44 项通过；Clippy、格式及差异检查通过。真实 Docker 登录管理集成另通过 1 项，覆盖 Pi/Grok 两种参数路径、材料保存、非交互拒绝、请求重放、取消、代次失效及所属资源回收，证据 `.agents/verify-runs/1/cli-login-2ecf4eac-7795-4f0d-88ea-453dfdb09027.json`。登录程序为明确合成替身，nativeLogin=false/model=false；未读取用户真实凭据。共享容器启动器回归另通过 1 项，证据 `.agents/verify-runs/1/cli-container-7f9674ba-751e-4f61-afb3-929b65c2bdf0.json`。

下一主路径：将专用登录材料与执行上下文绑定，接通运行服务的真实 CLI 投递、停止回收和能力检查；再验证真实 API/Pi/Grok、CLI/产品 Skill 团队路径，以及独立审查、CI、PR 与合入。上述局部测试不勾选完整 Story，不把已有材料当作真实认证通过。


## 2026-10-03：运行服务接入隔离 CLI

L1: reuse v0.10；L2: write v0.15。继续全部 58 项端到端目标，未冻结验收候选，未进行独立审查或创建 PR。

运行服务现按 Run 冻结连接进入 API 或 CLI 准备，复用成员通道、当前权限检查、停止观察和在途调用排空。CLI 先取得专用环境锁、核对登录材料元数据/固定镜像/引擎及上下文，再领取投递；启动事务核对 Worker、连接版本与登录代次。锁持有至本 Run 回收结束，登录刷新直接保留于 Worker 专用目录，不复制宿主或其他成员凭据。不同 Task/用途的会话卷独立，同一上下文跨投递续接；Skill 文件由当前授权工具生成、按 Run 私有保存并只读挂载。

生产资源初始保存 creationAuthorized=false；登记真实子进程后重查权限并提交创建许可，再发送私有容器参数。客户端在许可前中断可证明没有创建容器；旧记录没有该字段继续保守处理，缺 PID 仍未知。许可已发出时必须核对进程组及真实 Docker 所属容器/网络，未核对不释放 Run。新增测试覆盖错误登录代次/引擎拒绝、事务回滚、没有进程登记不能给许可，以及未给许可的中断无需 Docker 即可核对。

服务联合测试发现并修复成员通道只接受 API 握手的遗留约束；现在从 Run 冻结配置确定握手类型，拒绝成员自选类型。executionTransports 现包括 api/agent-cli，表示调度代码已接线；doctor 明示 CLI 原生模型待验证，connection show 的 executionSupported 不代表环境、账号或模型就绪。

开发回归：Rust 10 Unit + 19 CLI + 114 core = 143 项通过，11 项显式环境测试默认 ignored；TypeScript 44 项通过；Clippy、格式和差异检查通过。另显式运行：

- 实际 CLI/PTY/运行服务/SQLite/Docker/milkie 集成通过：工具进入核心、同 Task 两投递续接、不同 Task 会话隔离、停服回收全部所属资源。证据 `.agents/verify-runs/1/cli-runtime-192fa75e-91ff-4cd0-a5ce-cd1e15594c7b.json`。
- 专用登录成功/取消回收回归通过，证据 `.agents/verify-runs/1/cli-login-f87d6845-0b70-4f44-bc1b-aa6016625c70.json`。
- 实际 Docker 恢复回归通过：活进程不能提前回收、引擎变更保留未知、异主同名资源不删除；证据 `.agents/verify-runs/1/cli-recovery-44410785-e2e5-4b72-95be-3a22743ca3bb.json`。

以上模型和原生登录均为明确替身，不是 Story 通过证据。固定生产镜像中的真实 Pi --help / Grok login --help 在无网络、无宿主挂载下确认所用标志存在，未登录、未获取凭据、未调用模型。原生 Grok #270 心跳问题仍须实际复测，不能由上述 fixture 证明消失。

剩余：CLI 专用能力检查；旧投递 pending 的可信核对；真实 API/Pi/Grok 的认证、模型工具行为与完整游戏协作；CLI/产品 Skill 两条入口全部 L1.8 验收；随后独立审查、CI、PR 和合入。完整团队目标保持未完成。

## 2026-10-03：跨投递旧调用的只读核对

L1: reuse v0.10；L2: write v0.16；继续 Issue #1 的全部 58 项验收，仍处 implementation，未送独立审查。

新增可信适配私有消息 tool.reconcile，不在模型工具目录。核对始终绑定当前 Run，核心要求原 Run 已停止、原投递正确、Task/Worker/冻结执行配置/持久原生上下文相同，并重查当前成员与原工具权限。按原 fingerprint 返回已提交结果；无原记录返回 not_executed，不补执行、不写业务账本、不新增消息或消耗业务额度。普通工具入口也拒绝把旧 Run 未提交调用放到新 Run 执行。

API/CLI 生产入口在调用新模型前使用只读核对。其它投递的 pending 不再永久阻塞同一原生上下文，但仍保持原投递/操作身份；API 核对结果进入原生输入，CLI 同时核对 SDK 待回复调用与宿主账本。旧 pending 保持至后续原生轮次持久成功，再写 completed；核对后、输入保存前再次崩溃仍能重新只读查询。原生会话由 milkie 续接，不通过工作消息重建。恢复计数可跨多个旧 Run，受整个通道帧数边界约束；每个新 Run 的 100 次工具调用预算不变。

测试覆盖已提交/未提交、指纹或原投递不符、错误原生上下文、撤权、恢复前再次崩溃、同投递禁止补执行，以及 API/CLI 真实 SDK 在另一投递前核对后续接。真实服务集成通过显式故障注入将两个已提交调用的宿主记录恢复为 pending，随后由新投递沿只读通道取得原结果，继续处理并回收资源；模型和登录仍为明确替身。证据 `.agents/verify-runs/1/cli-runtime-a38dda23-19f3-469f-a295-ded730692a9b.json`，不作为原生模型或 Story 通过证明。

生产镜像已重新构建为 `sha256:a1d287d99fd390cb02a3e7a420ffe7c91105ebc8b58e63526189ec108777a3aa`，固定 milkie e049f0b、Pi 0.85.1、Grok 1.0.46；只打包应用与已核对 Linux 二进制，没有复制凭据/会话。上述真实服务测试使用该镜像派生的显式替身镜像。

Rust 10 Unit + 19 CLI + 115 core = 144 项通过，11 项显式环境测试默认 ignored；Clippy、格式和差异检查通过。TypeScript 全量 48 项通过，包含新增的只读通道、跨投递 SDK、API 恢复输入和崩溃窗口测试。

剩余主路径：CLI 专用能力检查、真实 API/Pi/Grok 认证及工具/恢复行为、CLI 与产品 Skill 的完整游戏团队路径和全部 L1.8 验收；随后独立审查、CI、PR 与合入。上游 #270 的真实 Grok 容器行为仍待复测，不能由替身续接证明通过。


## 2026-10-03：Worker 专用 CLI 连接检查

L1: reuse v0.10，58 项验收不变；L2: write v0.17。上一轮已提交跨投递恢复修复，本轮继续 implementation；没有冻结候选、Story 验收汇总通过或独立审查。

`connection test --worker` 接通实际隔离诊断：绑定原有执行配置、连接版本、登录代次、固定镜像、引擎和接入版本；使用该 Worker 的专用登录、新建临时原生会话和唯一无业务副作用工具。只有准确完成本次随机值的工具往返且原生执行成功才记录通过。缺环境、缺登录、环境变化和无工具回答分别保留固定失败类别；不创建 Task/Run/消息，也不输出模型正文或提供商异常。

CLI readiness 改按当前 Worker 展示在 `workerReadiness`，连接级别不再共享 ready；同连接另一成员不继承检查。修改说明仍适用，登录代次/镜像/引擎/接入变化使旧结果不适用。显式旧版本仅允许该 Worker 已有的不可变配置。产品 Skill 指导与 S1 功能地图已同步。

诊断资源先持久登记，可信创建进程归属和创建许可提交后才发送私有参数；与准备/登录/业务执行共用锁。正常退出、客户端强杀或服务核对均按真实进程组、引擎和资源所有权回收；未核对资源阻止该成员新操作。原请求只核对和重放，不再次请求模型；中断最终保存 probe_interrupted。停止后删除诊断原生会话，保留独立登录目录。

开发回归：Rust 14 Unit + 19 CLI + 115 core = 148 项通过，11 项显式环境测试默认 ignored；TypeScript 52 项通过；Clippy、格式和差异检查通过。已完成检查的缓存重放不获取环境锁，不因其它执行占用而失败；相应测试通过。新增测试覆盖成员就绪隔离、代次/镜像/引擎/接入失效、发布事务失败、无副作用、错误工具与纯文字回答拒绝、启动前取消。

实际 CLI/PTY/SQLite/Docker/milkie 联合测试通过：先缺登录失败，专用登录后完成诊断，原请求重放，强杀诊断客户端后离线核对，继续运行服务的消息投递/续接/跨投递核对及停服清理。证据 `.agents/verify-runs/1/cli-runtime-d22cc024-3615-4856-978c-1654add2b27f.json`；生产基础镜像为 `sha256:7f1ee0455ce24eabd91b1c09d2460538b190c49b24ab7ae6c0afa0dfd8bfbf66`。登录和 Pi 进程为明确替身，nativeCli=false/model=false；该检查不证明真实模型或完整 Story 通过。

剩余主路径：真实 API/Pi/Grok 认证、工具行为和原生续接，Grok #270 实际容器心跳复测，CLI 与产品 Skill 的完整游戏团队路径及全部 L1.8 验收，随后独立审查、CI、PR 与合入。所有必需项保持原范围。


## 2026-10-03：真实原生 CLI 登录联调与网络缺口

L1: reuse v0.10；L2: reuse v0.17。按 verify-atelier 的真实入口路径在独立开发工作区建立 Pi/Grok 连接和 Worker，显式 prepare/login/test；未冻结候选、未标 Story 通过。没有复制宿主登录材料或会话；仅参考本机非秘密的默认模型选择。

真实 Grok 1.0.46 首次拒绝同时传入 `--oauth --device-auth`。此前登录替身也错误接受了这个组合，模拟回归未覆盖原生参数互斥。已改为 `grok login --device-auth` 并同步替身约束；重新从产品入口启动后已进入真实设备认证请求，参数错误消失。实际 CLI/PTY/Docker 登录管理回归通过，证据 `.agents/verify-runs/1/cli-login-5d99ec69-feff-40e3-aced-c2fdade5f89d.json`；其中认证程序仍为明确替身，仅证明管理流程。

真实 Pi 0.85.1 已进入交互界面并选择 OpenAI Codex 设备认证。Grok 的真实请求在连接 auth.x.ai 时失败；Pi 的请求收到 HTTP 403 / unsupported_country_region_territory。均未完成认证、未取得真实模型成功。Pi 退出时因原生程序创建 auth.json 且正常退出，管理记录为 login_material_saved_unchecked；随后的实际 connection test 正确返回 cli_native_failed，没有将文件存在当作可用认证。

网络对照：本机直连 Grok 认证地址超时，使用已配置的本机 HTTP 代理返回 HTTP 200；在该代理上固定解析出的数值目标地址同样返回 HTTP 200。当前受控出站代理只实现直接 TCP 出站，尚未提供显式的上游网络代理配置，因而隔离容器不具备与本机 CLI 相同的网络路径。下一步应在冻结连接配置中明确此网络路径，并继续由受控出站代理限制精确域名/443、验证公开地址和固定目标 IP；不能把宿主代理环境变量直接交给成员容器来绕过约束。

本轮生产镜像重新构建为 `sha256:3173ff5f9c70007aab51590d662c86a886e902fa2483457353d7cf6628122f3a`。真实认证尝试、返回类别与资源核对记录在 `.agents/verify-runs/1/native-cli-auth-20261003.json`；本次所属容器及网络均已清理。专用工作区保留连接、成员及失败历史供继续修复；不保留登录终端全文，不含认证成功或业务产出声明。

milkie #270 仍 OPEN，未进行已认证的 Grok 执行，不能判其心跳问题解决。当前 API 配置仍为此前 glm-latest/coding 端点，已询问用户订阅是否修复或改用何连接；未盲目重复失败请求。下一步仍是显式网络路径、真实认证和模型工具/续接，再完成两条入口的完整游戏团队与全部 58 项验收，之后独立审查、CI、PR 与合入。


## 2026-10-03：CLI 显式上游网络代理与原生设备认证

L1: reuse v0.10，58 项验收不变；L2: write v0.18。继续 implementation，尚无冻结候选或 Story 验收通过。

agent-cli 新增可选 egress_proxy，以固定 IPv4 和端口表达无认证 HTTP CONNECT 上游。连接配置换代和 Run 资源均保留冻结路由，旧配置省略字段仍保留原序列化身份。登录、连接诊断与执行的可信私有启动参数均接通此配置，不把宿主环境变量或基础设施地址交给成员。出站代理先验证精确域名/443 及全部公开 DNS 答案，再提交数值目标 CONNECT；拒绝代理认证、重定向和异常响应，失败不回退直连，TLS 保持端到端。

开发检查：Rust 14 Unit + 19 CLI + 116 core = 149 项通过，11 项环境测试默认 ignored；TypeScript 54 项通过；Clippy、格式和差异检查通过。新增覆盖路由字段校验、配置换代后的旧 Run 持久路由、目标校验先于拨号、分段响应头、代理认证/重定向/超限响应拒绝及无回退。

真实 Docker 网络检查两条路径通过：直接出站证据 `.agents/verify-runs/1/isolation-c045f0c4-96c8-45f4-b2f6-1ab36f59458b.json`；显式上游证据 `.agents/verify-runs/1/isolation-a32ba275-70bd-4f57-9c2c-7a7c44f273b6.json`。后一条额外确认成员无法直连上游、上游仅收到数值 IP CONNECT，同时批准目标的真实 HTTPS 返回成功。

重建生产镜像 `sha256:2c6218fb506dbc070c1771492ede209f7d096191537928e4bed3012c2cc853a4`，固定 milkie e049f0b、Pi 0.85.1、Grok 1.0.46。实际 CLI/PTY/SQLite/Docker/milkie 服务回归通过，证据 `.agents/verify-runs/1/cli-runtime-7176fc65-70a5-4fa7-9b91-b37c3019a04d.json`；登录和原生进程为明确替身，不证明模型或 Story 通过。

另通过真实产品 CLI 更新独立验证工作区的 Pi/Grok 连接、Worker 绑定和专用准备，再启动真实 Grok 设备认证。已成功获得 xAI 设备授权入口，此前认证网络超时消失；等待用户完成原生账号授权，尚未宣称登录或模型可用。没有复制宿主登录材料、没有创建业务任务。设备码和私有网络配置仅在本地忽略目录/临时交互中使用，不纳入设计或提交。

当前优先顺序：完成两种原生专用登录和模型工具诊断、真实续接及 Grok #270 复测；真实 API 仍待确认订阅修复或可用连接；再跑 CLI/Skill 完整游戏团队和全部 L1.8，之后独立审查、CI、PR 与合入。上游 Issue 是否关闭不代替实际消费验证。


## 2026-10-03：基础 CI 与等待原生授权

沿用 L1 v0.10 / L2 v0.18。新增 GitHub Actions 基础检查，在 macOS 固定 Node 23.11.0、Rust 1.91.1 和 milkie e049f0b；检出公开上游的固定快照并按现有脚本构建包，npm lock 校验包完整性，先构建和测试 TypeScript，再运行 Rust 格式、Clippy 与全目标测试。工作流只读仓库权限，不持久保存 checkout 凭据，不配置提供商秘密。CI 运行结果另记，不因文件存在宣称通过。

CI 不执行默认 ignored 的真实 Keychain/Docker 环境测试，也不替代原生模型和 CLI/Skill 全团队验收。原生 Grok 设备认证进程本轮核对仍在等待授权；Pi 同工作区并发登录被既有环境互斥拒绝，没有启动第二次原生认证，须待 Grok 完成或结束后继续。


基础 CI 首次运行 `37108533107` 在 npm ci 被 EINTEGRITY 拒绝，Rust 检查未执行。第二次 `37108717165` 复现并保存公开依赖包用于比较。对 CI 与本地产物逐文件和完整解压 tar 核对：tar 都为 1,965,568 字节，SHA-256 均为 `03f8f60950ac5e071fe8bd42b766e044e476633aa8b802cfc96c2a76796e037d`，仅 gzip 编码不同。准备脚本现保留 tar 原样，以无压缩 DEFLATE 固定 gzip，并同时记录 tar/包摘要；只更新该本地依赖的 lock integrity，不改变上游文件、依赖版本或放宽校验。标准化包为 1,965,881 字节，包 SHA-256 为 `2f025c8d7f9f75d6f4e693d9e8088b469dd325cf5c8b95e6a4c38cc506e013a9`。修复后的 CI 结果待运行，不能把此前失败写为通过。


## 2026-10-03：基础 CI 通过与真实 Grok 工具往返

L1 v0.10 / L2 v0.18 不变。提交 d640ea2 的 GitHub Actions `37108902922` 在干净 macOS 环境通过：固定源码包生成与 npm integrity 校验、54 项 TypeScript、149 项 Rust、格式和 Clippy。11 项需环境测试仍默认 ignored；该结果不替代真实团队验收。CI 包摘要与本地标准化包一致，证据 `.agents/verify-runs/1/ci-37108902922.json`。

用户明确完成专用 Grok 设备授权后，真实 Grok 1.0.46 报告登录成功，管理入口保存登录代次 2 并核对资源全部停止。随后通过真实产品 `connection test --worker`，grok-4.7-build-fast 实际完成随机挑战的受控工具往返，记录 passed / cli_tool_roundtrip，诊断所属容器和网络已清理。证据 `.agents/verify-runs/1/native-grok-success-20261003.json`；没有保存账号标识、秘密或登录终端全文。该事实证明当前连接的认证和一次真实工具诊断，不证明原生续接、完整游戏交付或 #270 的偶发问题已解决。

Pi 0.85.1 也已通过同一路由进入 OpenAI Codex 设备认证，当前等待用户授权。工作区已建立本人、Grok 团队负责人、Pi 执行成员和独立 Grok 检验成员，共四名 Worker；职责和显式授权已保存，样例固定基线与可信检查配置已准备。独立检验成员尚须自己的专用登录，不共享团队负责人凭据；服务未启动、未代成员承接或安排工作。

真实 API 已按用户“再试一次”通过产品 CLI/Keychain 重测，当前配置仍返回 HTTP 400 / provider_rejected，不能声称 API 恢复。继续完成 Pi 与独立检验成员认证、实际业务执行/续接和完整 CLI/Skill 团队验收；全部必需范围保持不变。


## 2026-10-03：三名原生成员认证通过，真实游戏暴露工具名冲突

L1: reuse v0.10；L2: v0.19（纯技术边界补充）。Pi 执行成员和独立 Grok 检验成员在用户分别授权后，均通过自己的原生登录和真实随机工具挑战；与先前 Grok 负责人一起，三个成员已分别认证。证据为 `.agents/verify-runs/1/native-pi-success-20261003.json` 与 `native-grok-verifier-success-20261003.json`，不包含凭据或会话正文。

通过产品 CLI 配置固定井字棋缺陷输入与可信检查后启动服务。Grok 负责人真实读取消息、承接并安排 Pi 执行，随后对失败消息自主安排返工；宿主没有代成员作业务决定。Pi 的业务工具表含 milkie 保留名称 `read_file`，因此在 SDK 创建实际 Run 前被拒绝，未调用文件工具、未提交产出或交接。核心仅固定未改动基线的 partial 历史；这不是检验失败，也不算成功交付。停服后核对 activeRuns=0，任务与 queued 消息保留。开发证据 `native-game-setup-20261003.json`、`native-game-observations-20261003.jsonl`、`native-game-failure-20261003.json`。

修复仅在 CLI 接入将 `read_file` 暴露为 `atelier_read_file`；Skill 与核心账本使用规范操作名，每轮原生输入说明映射。回调校验后还原名称，恢复使用同一映射；禁止别名碰撞、旧原生名调用，撤权后仍重查。新增跨 Rust/TypeScript 集成直接使用核心生成的负责人、代码执行者、代码检验者工具表经过真实 milkie SDK；原生 CLI 为确定性替身，不算模型验收。另保存 CLI terminal 到资源记录，`run show` 和停止原因区分执行失败与资源回收，修正 runtime status 中过时的“尚未接入”说明。

开发回归：56 项 TypeScript、150 项 Rust（14 Unit + 19 CLI + 117 core）通过，11 项环境测试默认 ignored；格式、Clippy 通过。镜像重建为 `sha256:f2a0d78b55cb5b3afdff90887582f0baecec23d3eb321947a8712ac8ea2346eb`。新镜像须使用新冻结执行配置，不覆盖旧任务绑定；旧认证成功不能替代新配置的 prepare/login/test。完整服务联合测试与修复后的原生团队路径另记结果。API 仍为 provider_rejected；CLI 与 Skill 全部 L1.8、独立审查和合入尚未完成。

实际 CLI/PTY/SQLite/Docker/milkie 联合回归通过，证据 `.agents/verify-runs/1/cli-runtime-7ffa8b69-4067-4442-b5fd-d8f3430c4ce6.json`。新增核对 `run show` 保存 completed terminal、资源停止和可见停止原因；同一测试继续覆盖诊断客户端崩溃核对、跨消息原生续接与只读恢复、任务隔离和停服清理。该测试明确使用登录/Pi 协议替身，nativeCli=false/model=false。


## 2026-10-03：修复候选 CI、网络恢复与真实宿主入口

提交 `6526031` 的 CI `37124736634` 已通过，证据 `.agents/verify-runs/1/ci-37124736634.json`。同时只读核对生产镜像的 36 个接入文件与当前构建逐文件 SHA-256 相同，证据 `fixed-tools-image-provenance-20261003.json`；镜像一致性不代表原生验收通过。

修复后的新成员配置首次登录被当前网络的 Fake-IP DNS 拒绝。没有放宽公网目标校验、修改系统代理设置或复制凭据；只读排查后，认证域名的实际解析恢复为公网地址，上游仍可连接。独立 Grok 检验成员再次经用户授权完成新环境登录，并通过真实模型随机工具挑战，资源已回收；证据 `fixed-tools-grok-success-20261003.json`。Pi 新环境已进入设备认证，当前等待用户完成授权；不能沿用旧镜像的诊断结果。

在实际 Codex 会话中通过 `skill install` 安装并读取产品入口，按 `skill describe --protocol 2` 读取本人身份、团队及任务权限。经真实 CLI 取消旧镜像失败任务并保留历史，再提交相同井字棋目标、固定输入和检查契约的新 pending Task；没有代数字负责人承接或安排。证据 `fixed-tools-skill-game-setup-20261003.json`，completeSkillAcceptance=false：本次仅证明该宿主已加载入口并实际创建/配置任务，未覆盖从空工作区组队、退出宿主、完整交付或正式人类验收。

真实入口检查发现 setup 操作索引遗漏 connection login，且仍含“完整 CLI 接入仍须实现”的旧说明。现改为 prepare/login/test 的实际流程，并明确准备或登录成功不等于成员模型检查通过。仅更新产品指导文字，不改变权限、命令行为或冻结执行镜像。


## 2026-10-03：真实 Pi/Grok 团队首次完成游戏交付与人类验收

软件候选 `60437b06db0e07fb65c53156c0628b786596a8dd`，L1 v0.10 / L2 v0.19。Pi 新环境登录代次 3 成功并通过真实工具诊断；Grok 独立检验成员的新镜像检查通过，团队负责人保留的旧冻结配置也经显式版本检查通过。随后由实际产品 Skill 宿主启动运行服务，未代数字员工承接、安排、修改候选、交接或提交检验。

真实任务 `fec018c7-6072-4b9c-b972-82f001420a60` 修订 3：Grok 团队负责人自行承接并安排 Pi；Pi 经受控文件工具修复两条对角线漏判、更新中文说明，自测 19/19 通过后提交并直接交接。核心核对执行资源停止，固定非 partial 产出 `4b092657a2e9b276863b67976a063cbbc287f7c0c5896bc9769084820b1a481d`，内容摘要 `995d5a7dc3a44baf41d0d0acfbab5021d066cf345e2b658a2781512dc3169557`。独立 Grok 成员接受交接，针对该版本重新运行冻结检查，19/19 通过并提交检验 `803d50e8-c52c-4fc4-aa2f-591158b5ad4f`；没有以自测替代独立检查。

Grok 团队负责人分别处理执行结果和检验结果，通过原生上下文续接创建本人验收待办 `cdf64455-d53c-47d0-b815-a8e7140e1370`。实际产品导出后逐文件摘要匹配；Ego 浏览器以 file URL 打开同一版本、关闭网络，通过实际点击完成对角线胜利、平局及各自重开。用户在展示当前固定版本与依据后明确回复“接受这次小游戏交付”。产品 Skill 按请求修订 1、任务修订 3 提交接受并保存脱敏 decision-ref；独立进程查询确认请求 accepted、Task closed/outcome=accepted、任务修订 4，activeRuns=0、queuedDeliveries=0，未发布。

证据：`.agents/verify-runs/1/fixed-tools-game-observations-20261003.jsonl`、`fixed-tools-game-result-20261003.json`、`game-browser-4b092657.json`、`game-4b092657.png`。这些是当前真实链路的开发证据，不汇总任何 Story 为全部通过：本轮未退出 Codex 宿主，缺少独立 CLI/Skill 两套完整任务及从空工作区的 Skill 路径；Pi 首次交接前修复缺陷，未覆盖独立失败后的返工；API 仍无成功业务执行，其余权限/异常子项、独立审查和合入仍须完成。
