---
name: verify-atelier
description: Verify Atelier L1.8 through both the real CLI and the product Atelier Skill in a coding-agent host. Foundational CLI only; full team Drive remains blocked until runtime and member integration exist.
---

# Atelier CLI 与 Skill 验证手册

状态：基础 CLI 与产品 Atelier Skill 安装/描述已有实现，完整团队尚未交付。本手册是开发验收用途，不是产品 Skill；先读 [功能地图](features/README.md)、[L1.8](../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 与 [当前实现范围](../../../docs/implementation/1-first-team-delivery.md)。基础集成测试不代表产品验收通过。

## Launch

基础入口：`cargo build --locked` 后运行 `target/debug/atelier --help`，版本 0.1.0、JSON v2。使用专用空目录，通过 `--workspace <目录>` 选择范围；`workspace init --name <本人名称>` 初始化，写操作传 `--request-id`，更新传 `--revision`。运行 `cargo test --locked --test cli` 可检查跨进程基础路径，但不能把它当作真实宿主 Skill 验收。

当前可驾驶 workspace、worker、team、task 的创建/更新/查询及人类承接、execute/verify/rework 安排及 blocker list/show/resolve、acceptance request/show、recovery apply、accept/reject 和 artifact export，mailbox list/respond/retry、message send、request show、runtime start/status/stop/reconcile、sample prepare、doctor。服务已有单实例生命周期和 API 投递执行，真实 CLI/Node 启停与崩溃资源核对使用合成凭据验证。API connection test 已有持久诊断及真实失败/重放检查；agent CLI 已提供 prepare/login 管理入口，原生账号认证、能力检查与完整交付尚未验证或完成，完整 Drive 仍为 BLOCKED；不运行尚不存在的命令。井字棋缺陷样例可通过 sample prepare 准备；独立检查器校准说明见 trusted-checks/tic-tac-toe/README.md。真实成员产出、具名宿主/模型及完整 Skill 路径仍须补齐证据。产品入口为 skill install/describe，源包在 skills/atelier；临时目标安装并让实际宿主读取，再以 describe 返回的身份和权限操作，不能从文件存在推定已通过。

## Doctor

实际检查 CLI --version/doctor、模型连接、Docker 检查环境、样例和工作区；另确认产品 Skill 已在宿主被加载、运行服务实际可用，终端工具能提交安排、读取消息/投递/Run 结果。仅文件存在不算加载或可用。真实服务不可用则所需项 blocked/not_run。

## Drive

1. 直接 CLI：从空目录走配置、服务启动、任务投递、成员处理、检验、验收/取消与导出，并执行映射的失败/并发路径；记录文本/JSON、stderr 和退出码。
2. 产品 Skill：在实际宿主中由自然语言请求驱动真实 CLI，不用脚本调用 CLI 冒充 Skill E2E；观察它是否按事实解释结果、取得对应人类决定、超时先查询而非重放。提交后关闭宿主，由数字员工团队通过消息继续推进；另验证人类负责人等待，不允许宿主脚本代成员作决定。

两入口均为必需，不能只跑 Unit/Integration 或检查 SKILL.md 关键词。没有图形 UI 验收要求；旧桌面编号见 L1 迁移说明，不记本期 skip。

## Evidence

`.agents/verify-runs/1/` 保存软件候选、L1/L2 版本、宿主/模型/Skill、时间/环境、逐项结果、原始 CLI 及脱敏宿主调用、Team/Worker/Task/消息/投递/Run/产出/决定关联。保留失败注入条件，不提交密钥、真实业务数据或完整私有会话。未执行不填 pass。

## Cleanup

只终止本次测试所属执行和容器，按实际身份核对；清理隔离工作区和本次安装的测试 Skill，不动用户其它配置/Skill/源仓库。未知资源不盲删，证据保留。
