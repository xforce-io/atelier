# milkie 接入与出站代理（开发中）

该目录提供 Atelier 的 TypeScript API 单次执行适配与持久工具调用账本。任务、成员身份、权限、消息领取和业务状态仍归 Rust 核心；私有 JSON Lines 通道已接到绑定的 Rust 成员入口，并通过真实 Node 子进程集成测试；生产服务已装配、启动和核对 API 接入进程，真实模型与完整团队验收仍未通过。

## 固定依赖

使用 milkie 已提交版本 `7865ffcc14a8359a055e5e6e0998b56ab2160379`。该提交已有 AgentRuntime 内置工具白名单、稳定 toolCallId、Run 控制和原生 checkpoint。NPM 的 `@freemanxu/milkie@0.1.1` 包早于这些能力，不能以相同版本号当作兼容版本。

准备一个包含该提交的 milkie Git 仓库，在本目录运行：

```sh
node prepare-dependency.mjs /path/to/milkie
npm ci --no-audit --no-fund
npm test
```

准备脚本只导出固定提交，忽略来源工作目录的未提交修改；在本目录的独立临时目录按上游 lock 构建，再打包到忽略的 vendor 目录，不修改上游工作区。适配自身的 package-lock 固定本地包完整性及依赖。缺固定提交、构建失败或完整性不匹配时停止，不回退旧 NPM 包。

已在 macOS、Node 23.11.0 / npm 10.9.2 下连续构建两次，包 SHA-256 均为 `6a4de9e52702ebc2b511b68af810743637d29817b17fa41090f7186267a88eec`；原始来源记录保存在 vendor/provenance.json。其他平台/工具链的构建与完整性需再验证。该本地源码包不是新的上游发行版本。

## 执行边界

`executeApiTurn` 接收可信核心装配的身份、上下文、Skill、模型和工具表；不接受模型自选身份。API 使用 milkie AgentRuntime，全部内置工具关闭，不装配 subagent、宿主 shell 或动态 Skill。工具只通过 milkie 的实际派发校验后进入核心回调；伪造工具名不会绕过派发边界。

工具调用先将 originatingRunId、toolCallId、operationId 与请求内容持久保存，再转交核心。相同调用内容冲突拒绝，传输重发沿用原 operationId；即使已有结果也重新向核心查询，不能绕过当前授权。新 Run 中模型新生成的调用取得新 ID，不做自然语言去重。账本目录需位于核心持有的私有上下文目录下，并由核心保证当前投递只有一个执行所有者。

工具回复失联或在途取消会停止继续调用模型，保留未核对记录。下一轮先核对原请求，再将核对结果作为数据交给模型；无法核对时拒绝进入模型。取消等待不撤销核心已经提交的效果。

每次执行限制 15 分钟、50 次模型迭代及 100 次工具调用；适配边界额外拒绝第 101 次工具调用，明确报告预算耗尽。返回保留 milkie 原始终态和适配终止原因，不把执行正常结束当作消息已处理或任务已完成。原生 checkpoint 使用具备持久确认能力的事件存储；返回前确认当前 Run 的记录已持久。上下文归属的最终 Task/Worker/执行配置/用途校验由核心入口负责，适配另外核对 checkpoint 的 Worker 与 contextId。

## 已验证与缺口

25 个 TypeScript 测试（其中 22 个覆盖 API/通道/检查，3 个覆盖出站代理）使用真实固定版本 milkie Runtime、实际文件账本和原生 SQLite/checkpoint；模型响应为确定性 fixture。覆盖工具表、原生工具拒绝、丢回复后先恢复再调模型、跨 Run 去重边界、授权重查、取消、原生状态重开及 100 次工具调用上限。

Rust 另有 4 个跨进程集成测试，使用本目录构建的生产通道客户端与真实 Node 子进程，覆盖提交/重复请求、越界 ID/序号/截断输出、已提交但回复管道断开。这些子进程按 fixture 发起工具调用，不调用模型；正常 terminal 也不会将投递置为 handled。新增回应路径验证持久处理结果仍保持 claimed，资源确认停止后才由核心补记 handled。

macOS Keychain 和生产 API 服务已有真实 CLI/Node 启停及崩溃恢复测试，使用合成秘密和无效端点，不证明模型业务成功。API 连接检查已有专用无工具进程与真实 CLI/Keychain 的失败/重放测试，成功模型回答仍未取得。核心已补恢复/继续入口和产品 Skill 安装/描述；尚缺真实 API 模型成功及至少两种 CLI 的隔离运行、受控工具、登录和原生续接接入。这些仍属于 Issue #1 必需范围；本目录的测试不替代任一完整 Story 验收。

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

`egress-proxy.ts` 是 Atelier 拥有的受限 CONNECT 组件，不使用 milkie 未提交的 CLI 接口。`proxy-main.js` 只从私有 stdin 读取一次 `{listenHost, hosts}`，之后保持管道作为生命周期控制；listenHost 必须是本容器已有的具体 IPv4 地址。启动后只输出固定 listening 回执，控制管道关闭即退出。策略来自可信资源控制侧，不能由模型修改；错误不回显配置或正文。

精确 DNS 域名及 443 端口之外的 CONNECT 在解析前拒绝。每个 IPv4 DNS 答案都须为公开地址，选中的数值地址直接用于连接，不进行第二次解析；拒绝私网/回环/保留地址及混合答案。普通 HTTP 代理、IP 字面量、用户信息和含路径目标拒绝。该组件不解密 TLS，也不代替各 CLI 原生工具关闭和身份校验。

单元与真实 TCP 集成包含策略、绕过形式、DNS 混合答案、数值地址固定、转发、关闭及配置脱敏。另显式执行：

```sh
npm run test:isolation
```

此检查需要已有固定井字棋检查镜像中的 Node（仅作为可信测试运行时）及 Docker，不安装或运行 agent CLI。它创建带唯一标签的临时内部网络（internal + inhibit_ipv4、IPv6 关闭）、双网络代理，以及真实宿主网络 TCP 对照服务：普通 bridge 能访问对照服务，内部容器不能访问网关、其它宿主地址、直接公网或外部 DNS；经代理只可访问批准的 example.com:443 并取得实际 HTTPS 回应，未批准目标拒绝。之后关闭控制管道、核对代理停止并清理本测试资源。原始证据保存在 `.agents/verify-runs/1/isolation-*.json`，包括代理代码摘要和明确的 cliExecution=false。

2026-10-02 再次核对 milkie 远端 main 仍为 `7865ffcc14a8359a055e5e6e0998b56ab2160379`，#263 未关闭且相邻工作区实现未提交；草稿仅有标准/只读原生工具模式，未提供本项目必需的受控工具回调及隔离启动入口。本组件不消费该草稿，也不私造原生 CLI 协议。CLI 镜像、Worker 专用登录、原生会话卷、Run 资源账本及完整执行接线仍需完成。
