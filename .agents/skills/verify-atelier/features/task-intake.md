# S6 验证路径

状态：基础 CLI 部分已实现，完整产品路径未验收。标准为 [L1 v0.10 第 8 节](../../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 的 S6.A1–A5；本文件只提供路径，不复制另一套验收标准。

## 真实入口

提交目标生成 Task 与团队负责人投递；团队负责人实际作接受/等待/拒绝；pending 更新与旧决定冲突。

## 驾驶与证据

补充/取舍请求分别验证普通回复、正式回应、落实成功/受阻以及过期/无权；不以回应替代授权。

无 Git/Docker 的通用承接用可用 API 协调；验证任务与投递原子保存及请求去重，不靠用户搬运。

CLI 和产品 Atelier Skill 分别验证；需要真实成员的路径不可用 stub 或宿主脚本替代。版本、对象和原始脱敏证据对应 L1/L2 v0.10。基础 CLI 的可用命令和集成测试见[实现记录](../../../../docs/implementation/1-first-team-delivery.md)；服务、成员执行与产品 Skill 路径尚未齐备，完整验收仍 BLOCKED，不填 pass。

## 当前实现证据边界

普通补充事项已提供 CLI request/list/show/respond/record，测试覆盖回应与落实分离、实际操作引用、受阻与过期、权限及事务回滚。本机本人入口和绑定数字员工工具均已有集成测试；数字员工可查询、正式回应、补齐 pending 契约并引用实际操作完成核对，受阻后可由后续消息继续。真实模型、产品 Skill、验收及负责人恢复事项仍需完整取证，S6.A5 不标 pass。

数字员工 `task_intake` 与 `message_respond` 已有核心及管道集成测试：接受不结束协调责任，等待/拒绝与通知持久保存，处理结果与停止核对分离；代码输入校验走数据库线程之外的阻塞工作线程。当前调用由测试 fixture 选择，不是模型自主承接证据，S6 仍未完整验收。

开发集成入口已尝试真实 API 澄清，当前连接返回 HTTP 400 / InvalidSubscription，未生成补充事项，失败保留为 blocked。此入口不是产品入口，不能替代本文件要求的 CLI/Skill 验收。


2026-10-04 补证：`cli-decision-flow-20261004.json` 与 `skill-decision-flow-20261004.json` 记录直接 CLI 和实际 Pi 宿主普通回应/正式回应/受阻/落实/过期/无权闭环，S6.A5 的开发证据齐备；与游戏正式验收分开。`cli-disconnected-create-20261004.json` 配合事务回滚/并发 Integration 覆盖 S6.A4；新旧真实角色快照及不可变配置/显式刷新 Integration 覆盖 S6.A3。S6 全 Story 尚未验收，详见实现记录最新章节。


2026-10-04：实际 Pi Skill 在无 Git/Docker 的专用环境提交三个报告任务后退出，业务交由 DeepSeek 处理。API 随后 MODEL_BAD_RESPONSE，重测 HTTP 402；三类决定未齐，S6.A2 保持未通过。原任务/失败/本人恢复事项保留且服务停止，详见 skill-report-provider-failure-20261004.json。


2026-10-04：用户充值后原连接检查通过；实际 Pi Skill 从原待办恢复三个原 Task，仍以原上下文续接，但均触发空 assistant 消息 HTTP400（milkie #273）。全部资源停止、原记录保留，S6.A2 仍未通过，见 skill-report-funded-recovery-20261004.json。
