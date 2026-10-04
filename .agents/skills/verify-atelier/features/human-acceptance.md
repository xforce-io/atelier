# S4 验证路径

状态：验收及导出入口已实现，完整真实 CLI/Skill 路径未验收。标准为 [L1 v0.10 第 8 节](../../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 的 S4.A1–A8；本文件只提供路径，不复制另一套验收标准。

## 真实入口

本人收件箱待验收、查询依据、明确接受/拒绝、过期/无权拒绝和无覆盖导出。

## 驾驶与证据

覆盖拒绝旧请求、禁止同产出重开、返工新版与新检验/请求后接受的完整闭环。

数字员工不能代验收；关闭后的迟到消息不重开任务；证明消息已读不等于正式决定。

对 S4.A3/A4 分别核对旧产出检验、缺少有效独立检验、fail/inconclusive，以及普通说明声称已 review；从 CLI 与真实 Skill 尝试验收，保留拒绝和未关闭状态的证据。换消息、入口或请求 ID 不能绕过契约条件。

CLI 和产品 Atelier Skill 分别验证；需要真实成员的路径不可用 stub 或宿主脚本替代。版本、对象和原始脱敏证据对应 L1/L2 v0.10。当前 CLI/受控成员入口已有 Integration；真实模型业务及产品 Skill 的明确人类决定路径尚未验收，BLOCKED，不填 pass。

## 当前入口与开发证据

- 人类团队负责人：`task acceptance request <Task> --revision <版本> --artifact <ID> --verification <ID> --summary <说明>`；数字团队负责人：受控 `acceptance_request` 后以 `message_respond(kind=decision)` 保存等待依据。请求不结束活动 Run，也不自动代答。
- 本人：`task accept/reject <Task> --revision <任务版本> --request <请求 ID> --request-revision <请求版本> --reason <决定依据>`，写操作带 `--request-id`；Skill 代调还须保存脱敏 `--decision-ref`。通过 `task decision show`、`task acceptance show` 分别查询事项和不可变接受/拒绝记录。
- 拒绝后：人类负责人 `task rework --rejection <请求 ID>`，数字负责人 `task_arrange` 引用 reason.kind=rejection；新产出、新独立检验后提出新验收请求。服务不自行返工。
- 取走结果：`artifact export <ID> --destination <新目录或空目录>`，父目录须存在，不能指向工作区内部。导出无需任务 requestId；检查返回的 contentDigest、partial 与清单，对照文件和可执行位；非空/链接目录拒绝，重复导出不覆盖。导出前后 Task/验收记录一致。

常规 Integration 已覆盖停止/授权/版本/独立证据门禁、事务失败整体回滚、拒绝后同产出重开拒绝、数字负责人接收拒绝后显式安排，以及部分产出导出、损坏内容拒绝和无覆盖。真实 Docker/CLI Integration 覆盖通过→人类拒绝→返工新版→独立通过→新请求接受，并核对当前/历史版本导出。候选和成员决定为 fixture，不能替代真实成员、真实 Skill 或实际游玩证据。完整 S4 仍未验收。


## 当前游戏的人类验收（2026-10-03）

候选 `60437b0` 的真实 Pi/Grok 任务 `fec018c7…` 已实际导出固定产出 `4b092657…`；独立检查 19/19，通过断网浏览器的胜利、平局和重开路径。用户在看到当前请求与依据后明确接受，产品 Skill 以任务修订 3、请求 `cdf64455…` 修订 1 和脱敏 decision-ref 提交；独立查询确认验收 accepted、任务 closed/outcome=accepted，运行数和排队数均为 0。见 `fixed-tools-game-result-20261003.json` 与 `game-browser-4b092657.json`。该次接受不替代两入口的拒绝/过期/撤权等剩余验收，也不代表整个 S4 通过。

2026-10-03 补证：新 DeepSeek 与职责交换后的 Grok 交付仍保留各自 open 验收请求，没有沿用首次接受。真实 Skill 对两份当前导出的非空目录再次导出，核心拒绝且原文件摘要不变；另将 API 失败任务的 partial 产出导出到新目录，清单/摘要一致、partial 明确保留、Task 与验收不变。证据 `skill-role-boundary-and-export-20261003.json`、`skill-partial-export-20261003.json`；仅证明这些导出和等待分支。

S4.A5 的撤权、数字负责人尝试 acceptance_decide、成员伪造 actor/跨任务参数已核对具体拒绝断言。S4.A8 将首次明确接受、真实宿主离开后返回待办与关闭后迟到消息测试合并：迟到普通消息拒绝，重复停止观测不改变修订，重开数据库仍为 closed/accepted。155 项 Rust 回归通过，开发证据齐备；真实人类拒绝与新版接受、Skill 过期决定仍未完成。
