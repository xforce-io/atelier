# Atelier · 工坊

人和数字员工共同工作的地方。

Atelier 是一个独立项目，面向人和数字员工的持续团队协作。它以任务契约组织工作，将讨论、委派、执行、交接、检验和验收连接成可观察的交付过程。

Atelier 以 Worker 统一表达 Human Worker 和 Agent Worker（数字员工），以 Task 组织工作，以 Team 组织持续交付。每个 Team 必须有且只有一名团队负责人，由人或数字员工担任，成员按职责直接协作。任务契约、产出、检验与验收分别保留记录。 Task 可交给一个 Worker 或一个 Team；当前对话最多聚焦一个 Task，切换不结束原工作，成员与团队可以持续承担多项任务。

首阶段提供 Rust 核心与 CLI、Atelier Skill，TypeScript 用于 milkie 接入；GPUI 桌面界面后续补充。milkie 提供底层 agent runtime，通过进程协议接入；Atelier 保有工作与协作状态，并为不同 coding agent 保留能力边界。

## 当前状态

项目处于产品与技术设计阶段，尚无可运行实现。文档描述目标方向，不代表已有产品能力或执行器认证。

## 文档

- [产品方向与研究背景](docs/overview.md)
- [L1 产品设计：首项 CLI + Skill 交付](docs/design/1-first-team-delivery/product.md)
- [L2 技术设计：首项 CLI + Skill 交付](docs/design/1-first-team-delivery/technical.md)
- [名词表](docs/glossary.md)
- [仓库协作规则](AGENTS.md)

## 首个验证场景

人把一项代码修改目标交给 Team，由唯一团队负责人组织承接并明确任务负责人，数字员工执行、另一名数字员工独立检验，人验收交付。验证包含成员直接交接、阻塞处理及检验失败后的返工；任务不会因会话或单次执行结束而失去状态，检验对应当前版本，人无需逐条转发消息。

首项交付以 CLI 和 Atelier Skill 为入口，从空工作区创建本人、两个数字员工及 Team，再建立任务、执行、独立检验、返工、人工验收与导出。Skill 引导和编排，核心执行权限、版本与验收规则；CLI 不依赖 Skill 也能独立操作。执行命令前台运行，Task 跨调用持久保存，不承诺终端关闭后的后台推进。

后续 [GPUI 产品参考](docs/design/future-gpui-product.md) 与 [技术参考](docs/design/future-gpui-technical.md) 保留桌面方向，不属于 Issue #1 的当前验收。本轮仅为设计，CLI 和产品 Skill 尚未实现。
