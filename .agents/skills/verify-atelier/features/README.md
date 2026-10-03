# 功能地图

Issue #1：异步团队、CLI 与产品 Atelier Skill，L1 v0.10 / L2 v0.20；基础 CLI、Worker 专用准备与登录管理及 CLI 运行服务接线、Worker 专用连接检查已有实现，三名原生 CLI 成员的专用认证和模型工具诊断已通过，真实 Pi 执行、Grok 独立检验及数字负责人发起人类验收已贯通，该游戏已获人类明确接受，完整验收仍未完成，全部 Story 尚未验收通过。[实现记录](../../../../docs/implementation/1-first-team-delivery.md) 区分已有集成测试与未运行的完整验收。旧前台流程证据不证明消息协作通过。

| 文件 | L1.8 | 真实入口 |
|---|---|---|
| [setup-and-intake.md](setup-and-intake.md) | S1 / A1–A3、A7–A17 | 产品 Skill 安装/加载/按权限 describe、初始化、配置本人及三名数字员工、指定人或数字员工团队负责人、授权、隔离 CLI 准备/登录和运行服务启动。 |
| [task-intake.md](task-intake.md) | S6 / A1–A5 | 提交目标生成 Task 与团队负责人投递；团队负责人实际作接受/等待/拒绝；pending 更新与旧决定冲突。 |
| [code-delivery.md](code-delivery.md) | S2 / A1、A3–A9 | 安排入箱、服务领取、数字员工实际执行、固定产出和阶段回复；API 与两种 CLI 分别覆盖。 |
| [verification-rework.md](verification-rework.md) | S3 / A1–A7 | 执行者获准直接交接或团队负责人安排；检验者接收/拒收、失败、团队负责人通过 CLI/成员工具有限返工、预留与消费、新版重验、inconclusive 终局后的明确新交接。 |
| [human-acceptance.md](human-acceptance.md) | S4 / A1–A8 | 本人收件箱待验收、请求/决定查询、明确接受/拒绝、拒绝后新版验收、过期/无权拒绝和指定版本无覆盖导出。 |
| [failure-and-lifecycle.md](failure-and-lifecycle.md) | S5 / A1、A2、A4、A5、A7–A13 | 客户端退出、服务停止/崩溃、重复投递、撤权、正式阻塞报告/解决、按投递终局显式 retry、本人恢复选择/落实、runtime reconcile 可信资源核对（含 CLI 资源归属与引擎一致性）与任务取消。 |
| [asynchronous-team.md](asynchronous-team.md) | S7 / A1–A5 | 数字团队不依赖宿主阶段调用，从提交目标持续处理工作消息到人类待办；含普通说明与消息循环。 |
