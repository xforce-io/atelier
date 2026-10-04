# milkie 接入与出站代理（开发中）

该目录提供 Atelier 的 TypeScript API/CLI 单次执行适配与持久工具调用账本。任务、成员身份、权限、消息领取和业务状态仍归 Rust 核心；私有 JSON Lines 通道已接到绑定的 Rust 成员入口，并通过真实 Node 子进程集成测试；生产服务已装配、启动和核对 API 接入进程，真实模型与完整团队验收仍未通过。

## 固定依赖

使用 milkie 修复提交 `da7767790bcb38e30aa49910ad05fba3670689ca`（已通过 [milkie PR #274](https://github.com/xforce-io/milkie/pull/274) 合入主分支，固定的是其变更提交）。本次增加失败 checkpoint 续接的空 assistant 请求投影修复，checkpoint 格式与有效历史不变。该提交已有 AgentRuntime 内置工具白名单、稳定 toolCallId、Run 控制和原生 checkpoint。NPM 的 `@freemanxu/milkie@0.1.1` 包早于这些能力，不能以相同版本号当作兼容版本。

准备一个包含该提交的 milkie Git 仓库，在本目录运行：

```sh
node prepare-dependency.mjs /path/to/milkie
npm ci --no-audit --no-fund
npm test
```

准备脚本只导出固定提交，忽略来源工作目录的未提交修改；在本目录的独立临时目录按上游 lock 构建，再打包到忽略的 vendor 目录，不修改上游工作区。适配自身的 package-lock 固定本地包完整性及依赖。缺固定提交、构建失败或完整性不匹配时停止，不回退旧 NPM 包。

已在 macOS、Node 23.11.0 / npm 10.9.2 下连续构建两次，包 SHA-256 均为 `6a4de9e52702ebc2b511b68af810743637d29817b17fa41090f7186267a88eec`；原始来源记录保存在 vendor/provenance.json。其他平台/工具链的构建与完整性需再验证。该本地源码包不是新的上游发行版本。

2026-10-03 的干净 CI 发现官方 Node 与 Homebrew Node 对同一 tar 的 gzip 压缩结果不同，导致原 npm integrity 拒绝安装。当前准备脚本保持 npm 产出的 tar 原样，用无压缩 DEFLATE 统一 gzip 编码，同时记录包摘要和 tar 摘要；锁文件继续严格校验，不在 CI 动态改锁。固定 e049f0b 的 tar SHA-256 为 `03f8f60950ac5e071fe8bd42b766e044e476633aa8b802cfc96c2a76796e037d`，标准化包 SHA-256 为 `2f025c8d7f9f75d6f4e693d9e8088b469dd325cf5c8b95e6a4c38cc506e013a9`。这次只改变压缩编码，依赖文件内容不变。

2026-10-04 固定修复提交 `da77677` 的 tar SHA-256 为 `cffb71e4ccec9b7590dbc0ff1081bd1775acef7df1c508d969980d4ffc6c761c`，标准化包 SHA-256 为 `1d1287006a2b7449daf05f3fef4c5ecb4e9de71dadeee9e8f1c92c4add28a820`；以上旧摘要仅保留历史，不用于当前锁文件。

## 执行边界

`executeApiTurn` 接收可信核心装配的身份、上下文、Skill、模型和工具表；不接受模型自选身份。API 使用 milkie AgentRuntime，全部内置工具关闭，不装配 subagent、宿主 shell 或动态 Skill。工具只通过 milkie 的实际派发校验后进入核心回调；伪造工具名不会绕过派发边界。

工具调用先将 originatingRunId、toolCallId、operationId 与请求内容持久保存，再转交核心。相同调用内容冲突拒绝，传输重发沿用原 operationId；即使已有结果也重新向核心查询，不能绕过当前授权。新 Run 中模型新生成的调用取得新 ID，不做自然语言去重。API/CLI 的账本均按投递分目录，由核心保证上下文当前只有一个执行所有者；原生对话可跨投递续接，旧投递的操作不会作为新投递重放。旧开发布局的有效记录保留身份迁移；冲突、链接或其他投递的未核对记录均拒绝继续，不跳过文件后调用模型。

工具回复失联或在途取消会停止继续调用模型，保留未核对记录。下一轮先核对原请求，再将核对结果作为数据交给模型；无法核对时拒绝进入模型。取消等待不撤销核心已经提交的效果。

每次执行限制 15 分钟、50 次模型迭代及 100 次工具调用；适配边界额外拒绝第 101 次工具调用，明确报告预算耗尽。返回保留 milkie 原始终态和适配终止原因，不把执行正常结束当作消息已处理或任务已完成。原生 checkpoint 使用具备持久确认能力的事件存储；返回前确认当前 Run 的记录已持久。上下文归属的最终 Task/Worker/执行配置/用途校验由核心入口负责，适配另外核对 checkpoint 的 Worker 与 contextId。

## 已验证与缺口

TypeScript 开发测试使用真实固定版本 milkie Runtime、实际文件账本和原生 SQLite/checkpoint；模型响应为确定性 fixture。覆盖工具表、原生工具拒绝、丢回复后先恢复再调模型、跨 Run/投递的去重边界、授权重查、取消、原生状态重开及 100 次工具调用上限。最新执行数量与证据见[实现记录](../../docs/implementation/1-first-team-delivery.md)，不以历史计数代替当前结果。

Rust 另有 4 个跨进程集成测试，使用本目录构建的生产通道客户端与真实 Node 子进程，覆盖提交/重复请求、越界 ID/序号/截断输出、已提交但回复管道断开。这些子进程按 fixture 发起工具调用，不调用模型；正常 terminal 也不会将投递置为 handled。新增回应路径验证持久处理结果仍保持 claimed，资源确认停止后才由核心补记 handled。

macOS Keychain 和生产 API 服务已有真实 CLI/Node 启停及崩溃恢复测试，使用合成秘密和无效端点，不证明模型业务成功。API 连接检查已有专用无工具进程与真实 CLI/Keychain 的失败/重放测试，成功模型回答仍未取得。核心已补恢复/继续入口和产品 Skill 安装/描述；隔离 CLI、受控工具、登录管理和原生续接的接线已有开发集成；尚缺真实 API 模型成功及至少两种 CLI 的真实账号和模型验收。这些仍属于 Issue #1 必需范围；本目录的测试不替代任一完整 Story 验收。

## 私有进程入口

核心通过独占 stdin/stdout 管道驱动 `node dist/src/main.js --atelier-run <Run UUID>`，不用管理 CLI、通用 TCP 端点或模型指定的身份。hello/ready 校验固定 milkie 提交、API 能力与 Skill 摘要；start 提供绑定身份、任务数据、工具表和独立上下文。凭据仅通过此私有 start 传入，连接解析显式禁用宿主环境回退；不得将完整 start 写入日志或普通消息。

双向序号各自递增，同序号同内容可重传，不同内容或缺号拒绝。每帧最多 512 KiB、每方向最多 1024 帧和累计 32 MiB；进度/报告最多 16 KiB。握手与写入各限 10 秒；核心通道设 15 分钟期限，停止后给 10 秒报告终态，超时交回资源核对。关闭管道不撤销已进入数据库线程的操作。

核心持久保存 Task/Worker/冻结配置/用途族及职责快照的上下文绑定，不按每个 Run 新建会话。上下文使用标记与启动意图同事务保存；启动可能发生后，缺失目录不再自动补建。核心在启动前准备权限为 0700 的上下文和投递账本目录；适配拒绝符号链接、目录混用和已有上下文的隐式覆盖。上下文 manifest 固定 Task/Worker/执行配置/用途族/contextId，不包含凭据。`resume` 必须显式提供且找到匹配的原生 SQLite 和 checkpoint；缺失或身份不符就失败，不回退空白会话。接入进程只报告运行终态，不决定任务验收或投递完成。


## 开发期真实 API 检查

仓库根目录的 `examples/api_smoke.rs` 使用真实 API 与生产适配子进程，检查公开合成任务的澄清阶段。配置来自当前进程的 VOLCENGINE_TOKEN、VOLCENGINE_MODEL、VOLCENGINE_API_BASE，另设置 ATELIER_SMOKE_WORKSPACE 为新目录。令牌只经过私有管道传给子进程，不作为命令参数或连接配置保存。该入口不代表产品 CLI、产品 Skill 或完整团队验收。

2026-10-02 实测修复了显式 fields 与空 env 混用导致的装配拒绝；随后服务端返回 HTTP 400 / InvalidSubscription，尚无真实模型成功证据。生产进程的装配及启动前取消已有回归测试。详细范围与失败证据见[实现记录](../../docs/implementation/1-first-team-delivery.md)。


## API 连接检查

`probe-main.js` 只接收私有标准输入中的显式连接字段，不装配 AgentRuntime、Skill、工具或工作上下文。通过固定 milkie gateway 发送 64 token 上限的最小无工具请求；只返回固定状态/错误类别及可选 HTTP 状态，不返回模型内容、SDK 异常正文或秘密。进程自身 25 秒截止；Rust 控制整个观察在 29 秒内结束，并预留 1 秒回收实际 Child。父进程断管会使检查停止。

检查记录在外部请求前持久保存，同 requestId 不再启动检查。记录为 pending 只表示完成结果尚未保存，不承诺旧进程仍活着，也不伪造失败原因；显式新 requestId 可重测。当前 readiness 按不可变连接版本、凭据代次和接入版本核对，旧结果继续可查。成功检查不是业务授权或交付验收。

新增检查测试覆盖成功响应、空响应/伪工具调用拒绝、错误脱敏、显式凭据约束、真实子进程断管与截止时间；模型成功响应使用 fixture，不能升级真实模型验收。


## 出站代理与网络验证

`egress-proxy.ts` 是 Atelier 拥有的受限 CONNECT 组件，不使用 milkie 未提交的 CLI 接口。`proxy-main.js` 只从私有 stdin 读取一次 `{listenHost, hosts, upstream?}`，之后保持管道作为生命周期控制；listenHost 必须是本容器已有的具体 IPv4 地址。启动后只输出固定 listening 回执，控制管道关闭即退出。策略来自可信资源控制侧，不能由模型修改；错误不回显配置或正文。

精确 DNS 域名及 443 端口之外的 CONNECT 在解析前拒绝。每个 IPv4 DNS 答案都须为公开地址，选中的数值地址直接用于连接，不进行第二次解析；拒绝私网/回环/保留地址及混合答案。普通 HTTP 代理、IP 字面量、用户信息和含路径目标拒绝。该组件不解密 TLS，也不代替各 CLI 原生工具关闭和身份校验。

agent-cli 连接可显式指定 `egress_proxy: {"address":"规范 IPv4 地址","port":端口}`；配置随连接版本冻结，可信启动器将其作为 `upstream` 传给出站代理。省略即直接连接已核验目标，不读取宿主 HTTP_PROXY/HTTPS_PROXY。固定上游支持无认证 HTTP CONNECT，允许私网基础设施地址，拒绝 URL、域名、回环、链路本地、保留地址及额外字段。向上游提交已核验的数值目标 IP，保持精确域名/443 和公开地址校验；认证要求、重定向、异常响应均失败，不回退直连。登录、诊断和执行共用此路由，成员容器不获得上游地址或宿主代理配置。


单元与真实 TCP 集成包含策略、绕过形式、DNS 混合答案、数值地址固定、转发、关闭及配置脱敏。另显式执行：

```sh
npm run test:isolation
```

此检查分别覆盖直接出站和显式上游代理两种路径；后一条使用真实 HTTP CONNECT 对照服务，额外核对目标为数值 IP、成员不能直连上游。此检查需要已有固定井字棋检查镜像中的 Node（仅作为可信测试运行时）及 Docker，不安装或运行 agent CLI。它创建带唯一标签的临时内部网络（internal + inhibit_ipv4、IPv6 关闭）、双网络代理，以及真实宿主网络 TCP 对照服务：普通 bridge 能访问对照服务，内部容器不能访问网关、其它宿主地址、直接公网或外部 DNS；经代理只可访问批准的 example.com:443 并取得实际 HTTPS 回应，未批准目标拒绝。之后关闭控制管道、核对代理停止并清理本测试资源。原始证据保存在 `.agents/verify-runs/1/isolation-*.json`，包括代理代码摘要和明确的 cliExecution=false。

2026-10-02 再次核对 milkie 远端 main 仍为 `7865ffcc14a8359a055e5e6e0998b56ab2160379`，#263 未关闭且相邻工作区实现未提交；草稿仅有标准/只读原生工具模式，未提供本项目必需的受控工具回调及隔离启动入口。本组件不消费该草稿，也不私造原生 CLI 协议。CLI 镜像、Worker 专用登录、原生会话卷、Run 资源账本及完整执行接线仍需完成。


## CLI 接入开发进度（2026-10-03）

`cli-tools.ts` 保留完整工具 schema，只补充 enum/const 隐含类型，并通过 Ajv 在回调边界执行完整校验。`cli-turn.ts` 通过真实 ExecutionClient 串行转交获准调用，以稳定 callId 关联操作账本；先核对旧调用再续接，未曾转交核心的排队调用不会在恢复时首次执行。未知资源不能作为正常结束，丢失核心回复后停止 CLI，避免生成替代调用。

本层已接统一 SDK 与生产 `main.js` 私有进程入口；`agent-cli` 握手选择 CLI 分支，专用目录 manifest 绑定 Task/Worker/配置/用途及 SDK contextId，续接不重新建会话。生产 Rust 的 CLI 驱动、容器装配、专用登录及完整真实 CLI 路径仍待接线。新增测试中的 Pi 是明确标记的外部协议夹具，运行的 SDK、监督进程、Unix socket 和文件账本为真实实现；不能据此声明 Pi/Grok 模型、容器隔离或 Story 通过。


CLI 账本按投递分目录，原生上下文可以跨投递保留；同一投递重试重开原账本。存在其他投递的未回复调用时明确拒绝新模型执行，不改绑到新投递；由可信恢复流程先核对，相关服务接线尚未完成。

## 固定 Linux 镜像与容器启动器（开发入口）

本目录可从已准备的 Grok Linux arm64 二进制构建镜像：

```sh
node prepare-cli-image.mjs /path/to/grok-linux-arm64
npm run test:cli-container
```

脚本仅复制显式应用文件、固定 milkie 包与指定二进制到构建上下文，固定 Node 基础镜像和 Pi 0.85.1，并核对 Grok 二进制摘要。结果写入忽略的 `.cache/cli-image.json`，以镜像 digest 使用；脚本不导入任何登录或会话，仍为开发构建入口。

Rust `connection prepare` 核对上述本地固定镜像，并为指定 Worker/连接版本建立私有存储；`connection show` 可查持久准备结果。该入口尚不代替镜像构建脚本，专用 `connection login` 已接 TTY 容器、持久请求与中断资源核对，运行服务已接线，原生账号认证与模型业务仍待验证。准备结果明确为 `prepared_not_authenticated`，没有认证或模型成功含义。

`cli-container-main.js` 是可信宿主启动器，第一行私有输入携带核心已登记的 Run/工作区/归属、Docker 引擎、固定镜像、目录及批准域名，随后透传成员通道。仅创建具有归属标签的执行容器、代理与两个网络，核对非 root、只读根目录与受限挂载，成员容器不接出站网络。启动器不删除资源；核心须先登记其进程，再交付创建参数，停止时须核对其进程组消失并按资源身份清理。

显式 Docker 集成测试构建独立测试镜像，用清楚标记的 Pi 协议夹具替代 CLI 响应，通过真实启动器、Docker、milkie SDK 和私有管道验证工具请求/回复及退出；核对资源上限和只读 Skill 挂载，最后按标签核对并清理测试资源。该测试不读取账号、不调用真实模型，不代替两种真实 CLI 或完整团队验收。

`npm run test:cli-login` 通过 Python 3 创建真实 PTY，运行实际 Rust CLI 和 Docker。Pi/Grok 登录程序使用明确的合成替身，不读取用户登录；检查请求重放、代次、取消回收和无业务工作副作用，不证明原生认证通过。

运行服务现已复用成员通道调度 agent CLI，冻结登录代次和创建许可，挂载 Worker 登录目录与当前执行上下文。`npm run test:cli-runtime` 通过真实 CLI/PTY/Docker/服务验证受控工具、两轮投递续接、不同 Task 会话隔离和停服资源回收；原生 Pi 协议及登录使用明确替身，不能据此声称真实模型或两种 CLI 验收完成。

跨投递旧 pending 现经核心 `tool.reconcile` 只读核对，生产 API/CLI 不通过普通工具请求重做旧操作。已提交保留原结果，未提交明确返回 not_executed；错误上下文或撤权拒绝。旧 pending 在后续原生轮次持久成功后才标 completed，覆盖核对至输入保存之间的中断；原生会话仍由 milkie 续接，不从工作消息重建。

## CLI 连接检查

`cli-probe-launcher.js` 使用同一受限容器装配，单独的 probe 归属由 Rust 在启动前持久登记；`cli-probe-main.js` 在全新临时原生会话中用固定 milkie ExecutionClient 调用唯一诊断工具。必须准确回传本次随机值且原生执行成功，文字回答或声明能力不算通过。核心核对资源停止后保存结果，删除临时会话；专用登录独立保留。原请求和服务恢复只清理、从不重发模型调用。

结果按 Worker/执行配置/登录代次/镜像/引擎/接入版本适用，连接级别不共享 ready。SDK 聚焦测试和真实 CLI/Docker 集成使用明确协议替身，不是原生账号或模型验收。
