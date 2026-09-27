---
name: verify-atelier
description: Verify Atelier product paths against the accepted L1.8 through the real GPUI desktop entry. Currently design-only; block Drive until runnable application instructions exist.
---

# Atelier 桌面验证手册

状态：待实现。当前仓库没有可运行应用；本文件是设计阶段的路径映射，不是已完成驾驶证明。先读 [功能地图](features/README.md)、[L1.8](../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 及所对验收，不以核心测试替代桌面路径。

## Launch

实现后由应用仓库写入实际构建、启动、隔离工作区与测试样例准备命令，并固定软件、GPUI、milkie、模型和检查镜像版本。目前命令不存在，Drive 为 BLOCKED，不猜命令、不对真实业务目录执行。

## Doctor

须实际确认 macOS 应用可启动、测试工作区隔离、模型连接可用、检查环境与样例可用、凭据不会出现在记录中。缺真实服务只能进行明确的确定性边界测试，必需真实运行项保持 blocked/not_run。

## Drive

从 GPUI 实际入口执行每条适用 L1.8 子项。鼠标、键盘、中文输入、关闭窗口/重开、菜单退出是本产品入口，不能用 API 或 CLI 替换；错误注入在测试构建执行，标明注入条件。只有全部必需子项 pass 才汇总 S pass。

## Evidence

本地 `.agents/verify-runs/1/` 保存候选 SHA、L1/L2 版本、时间/环境、逐项结果、操作记录、必要截图、原始错误及对象版本关联。该目录不提交。标准留在 L1，执行结果留在运行目录；未运行不填 pass。

## Cleanup

关闭本次启动的应用与所属测试执行/容器，按真实资源归属清理隔离数据；未知资源不盲删。保留证据，不能卸载用户模型连接、删除源仓库或操作其它工作区。
