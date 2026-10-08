# 功能地图

Issue #1：异步团队、CLI 与产品 Atelier Skill，L1 v0.10 / L2 v0.28。2026-10-05 最终独立审查覆盖交付候选 `ad134f2` 的 124/124 文件，58/58 必需子项及要求分支闭合，PASS，无未决 P0–P2；[PR #2](https://github.com/xforce-io/atelier/pull/2) 已合入 main，Issue 已关闭。逐项结果及证据索引见 PR，历史过程见 [实现记录](../../../../docs/implementation/1-first-team-delivery.md)。历史证据保留原候选、SDK 与镜像归属，协议替身只证明接线，后续变更须重新核对适用性。

以下按日期保留历史开发观察，其中“未完成”不代表上述最终交付状态。

2026-10-04 逐项开发证据收集：58个必需项已收集历史开发证据，包括CLI新版0208ffae的明确接受与Task关闭；完整独立逐分支审计仍未完成，不能汇总为Story PASS。旧候选的独立审查确认3个P2和8文件/逐分支证据覆盖缺口；文件编码边界已在9928243修复。当时消费milkie #275 / PR #276的固定提交a3c1af0，以补齐CLI迭代预算及工具数据传输；新候选的接入、镜像、实际模型和证据适用性需分别核验，未运行项保持not_run/blocked。详见实现记录。

| 文件 | L1.8 | 真实入口 |
|---|---|---|
| [setup-and-intake.md](setup-and-intake.md) | S1 / A1–A3、A7–A18 | 产品 Skill 安装/加载/按权限 describe、初始化、配置本人及三名数字员工、指定人或数字员工团队负责人、授权、隔离 CLI 准备/登录和运行服务启动。重建后的二进制读不到已有 Keychain 项时，失败原因要求用当前二进制重新设置凭据。 |
| [task-intake.md](task-intake.md) | S6 / A1–A5 | 提交目标生成 Task 与团队负责人投递；团队负责人实际作接受/等待/拒绝；pending 更新与旧决定冲突。 |
| [code-delivery.md](code-delivery.md) | S2 / A1、A3–A9 | 安排入箱、服务领取、数字员工实际执行、固定产出和阶段回复；API 与两种 CLI 分别覆盖。 |
| [verification-rework.md](verification-rework.md) | S3 / A1–A7 | 执行者获准直接交接或团队负责人安排；检验者接收/拒收、失败、团队负责人通过 CLI/成员工具有限返工、预留与消费、新版重验、inconclusive 终局后的明确新交接。 |
| [human-acceptance.md](human-acceptance.md) | S4 / A1–A8 | 本人收件箱待验收、请求/决定查询、明确接受/拒绝、拒绝后新版验收、过期/无权拒绝和指定版本无覆盖导出。 |
| [failure-and-lifecycle.md](failure-and-lifecycle.md) | S5 / A1、A2、A4、A5、A7–A15 | 客户端退出、服务停止/崩溃、重复投递、撤权、正式阻塞报告/解决、按投递终局显式 retry、本人恢复选择/落实、runtime reconcile 可信资源核对（含 CLI 资源归属与引擎一致性）、checkpoint 写出前中断与任务取消。S5.A15 用既有 CLI 查看恢复事项的停止事实、产出对照和选项后果。 |
| [leader-continues-stop.md](leader-continues-stop.md) | Issue #20 / S1.A1–S5.A1 | 有新事实的停止交给团队负责人；同一种空停止不再自动排队；普通停住不新增恢复事项；已受理代码执行不能原样重试；第 3 次返工不被旧默认上限拒绝。 |
| [asynchronous-team.md](asynchronous-team.md) | S7 / A1–A5 | 数字团队不依赖宿主阶段调用，从提交目标持续处理工作消息到人类待办；含普通说明与消息循环。 |
| [deploy-duty.md](deploy-duty.md) | Issue #5 / S1.A1–A6 | 显式部署职责收到部署投递；验收记录与部署结果分开；未部署不关闭。无该职责的团队仍在接受后关闭。 |
| [host-environment-deploy.md](host-environment-deploy.md) | Issue #22 / S1.A1–S4.A5 | 登记本机部署目标、导出已验收产出、部署成员经确认执行本机命令、核心按登记方式核对后关闭为已部署。 |
| [readonly-workspace-ui.md](readonly-workspace-ui.md) | Issue #25 / S1.A1、S2.A1、S3.A1、S3.A2、S4.A1、S5.A1 | 本机 `view` 只读打开一个工作区，点一名工作成员看其发出和收到的工作消息。 |

冻结检查名称与检查记录 ID 的可发现性修复已分别由原真实任务和 Grok 负责人复核，详见实现记录；历史计数保留在对应日期小节，以本文顶部的逐项审计为当前状态。

2026-10-04 原报告任务续接、CLI第二次返工及两入口新版真实接受均已取证；当时依赖升级为milkie a3c1af0，历史真实原生证据保留原SDK/镜像绑定，升级本身不算新候选验收。

2026-10-05：当前固定消费 a8e4ea9（L2 v0.28），真实 Grok 预算耗尽终态及同会话续接复验通过，SDK 76项、Atelier 66项、Rust 161项及三项Docker接线检查通过。旧API代码产物未变，历史证据保留原候选/SDK；这些新组件检查不代替完整团队或独立审查。58项证据与当前候选适用性正在补审。
