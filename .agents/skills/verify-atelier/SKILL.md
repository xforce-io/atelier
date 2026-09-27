---
name: verify-atelier
description: Verify Atelier L1.8 through both the real CLI and the product Atelier Skill in a coding-agent host. Currently design-only; block Drive until runnable instructions exist.
---

# Atelier CLI 与 Skill 验证手册

状态：待实现。当前没有可运行 CLI 或产品 Atelier Skill。本手册是开发验收用途，不是产品 Skill；先读 [功能地图](features/README.md) 与 [L1.8](../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证)。

## Launch

实现后填入实际构建/安装、隔离工作区、公开样例、CLI/runtime/Skill/JSON 版本、具名宿主和模型，以及宿主加载 Skill 的可复现方法。当前尚无这些命令，Drive 为 BLOCKED，不运行猜测命令、不对真实业务目录测试。

## Doctor

实际检查 CLI --version/doctor、模型连接、Docker 检查环境、样例和工作区；另确认产品 Skill 已在宿主被加载、终端工具能执行长命令并读取结果。仅文件存在不算加载或可用。真实服务不可用则所需项 blocked/not_run。

## Drive

1. 直接 CLI：从空目录走配置、执行、检验、验收/取消与导出，并执行映射的失败/并发路径；记录文本/JSON、stderr 和退出码。
2. 产品 Skill：在实际宿主中由自然语言请求驱动真实 CLI，不用脚本调用 CLI 冒充 Skill E2E；观察它是否按事实解释结果、取得对应人类决定、超时先查询而非重放。

两入口均为必需，不能只跑 Unit/Integration 或检查 SKILL.md 关键词。没有图形 UI 验收要求；旧桌面编号见 L1 迁移说明，不记本期 skip。

## Evidence

`.agents/verify-runs/1/` 保存软件候选、L1/L2 版本、宿主/模型/Skill、时间/环境、逐项结果、原始 CLI 及脱敏宿主调用、Task/Run/产出/决定关联。保留失败注入条件，不提交密钥、真实业务数据或完整私有会话。未执行不填 pass。

## Cleanup

只终止本次测试所属执行和容器，按实际身份核对；清理隔离工作区和本次安装的测试 Skill，不动用户其它配置/Skill/源仓库。未知资源不盲删，证据保留。
