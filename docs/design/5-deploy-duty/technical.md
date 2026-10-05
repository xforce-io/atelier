# L2 技术设计：部署职责

版本：v0.1，2026-10-05；状态：Draft。依据：[L1 v0.1](product.md)、[Issue #5](https://github.com/xforce-io/atelier/issues/5)。不提升工作区 `user_version`。旧团队 JSON 没有 `deployer` 时视为没有部署职责。同日修订：提交部署结果时结束尚未领取的部署投递。

## 1. 数据

`Team` 与 `TeamPatch` 增加可选 `deployer`。`Permission::Deploy` 的持久名是 `task.deploy`。`Task.deploy` 为可选 `DeployRecord`：`state` 取 `open`、`blocked`、`succeeded`、`failed`，并保存 `acceptance_id` 与可选 `reason`。没有该字段的旧任务按没有部署记录读取。

任务 SQL `state` 仍只有 `pending`、`active`、`closed`。有部署职责时接受后保持 `active`，`outcome` 为空。成功后 `state=closed` 且 `outcome=deployed`。无部署职责时仍是 `outcome=accepted`。

## 2. 团队规则

`deployer` 必须是团队成员，且不能等于 `executor` 或 `verifier`。人类部署成员只能是本机本人。显示名不参与判断。更新时省略 `--deployer` 不清除已有职责。创建团队不自动授予 `task.deploy`。

## 3. 接受

无 `deployer`：保持现有关闭事务。

有 `deployer`：同一事务写入验收记录。若冻结与当前授权都包含 `task.deploy` 与 `task.communicate`，数字员工还有冻结执行配置，消息额度足够，数字员工还有 Run 额度，且当前修订没有未解决正式阻塞，则 `deploy.state=open` 并投递 `assignment.deploy`。任一条件不满足则 `deploy.state=blocked`，写入原因，不投递。两种情况都不关闭任务，也不把 `outcome` 写成 `accepted`。

## 4. 运行与提交

`assignment.deploy` 的 purpose 是 `deploy`，执行上下文族也是 `deploy`，不并入 coordinate、execute 或 verify。适配器启动参数接受该族。只有冻结部署成员、部署记录为 `open`、并同时持有当前与冻结的 `task.deploy` 和 `task.communicate` 时可以领取。撤销 `task.deploy` 会把尚未领取的部署投递标为阻塞。成员工具 `task_deploy` 只在该运行中提供，并写下本轮处理结果；随后停止不再另报一次失败。

提交成功：关闭任务，`outcome=deployed`，部署记录为 `succeeded`。提交失败：任务保持 `active`，`outcome` 为空，部署记录为 `failed`，向团队负责人投递一条 `failure`。已结束的部署拒绝再次提交。同一事务把尚未领取（`run_id` 为空且状态为 `queued` 或 `blocked`）的 `assignment.deploy` 标为 `handled`，原因是本次提交依据。已领取的部署运行不在这里改投递，仍由 `task_deploy` 的处理结果在停止后标为 `handled`。成功关闭时，其余仍排队或阻塞的投递继续取消。

本机本人在没有活动 Run 时用 `task deploy`。数字员工不能走管理入口。核心不派生部署进程，不调用 git，不读取 compose 文件。

## 5. 参与者

`participants` 在部署记录为 `open` 或 `failed` 时加入部署成员，使同一次部署运行能够结束。接受之前不因显示名或团队成员身份加入。失败后再次提交仍被拒绝。

## 6. 测试映射

| L1.8 | 检查 |
|---|---|
| S1.A1 | `deploy_duty_is_explicit_and_cannot_fold_into_execution_or_verification` |
| S1.A2 | 既有接受关闭测试保持有效 |
| S1.A3、S1.A5 | `deploy_success_closes_separately_from_acceptance`、`human_deploy_success_handles_the_unclaimed_assignment` |
| S1.A4 | `missing_deploy_grant_records_acceptance_without_closing_or_delivering` |
| S1.A6 | `deploy_failure_keeps_the_task_unfinished`、`human_deploy_failure_handles_the_unclaimed_assignment` |

真实 CLI 与产品 Skill 路径另记，不由上述核心测试代替。
