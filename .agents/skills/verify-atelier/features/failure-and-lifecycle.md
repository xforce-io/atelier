# S5 验证路径

状态：基础 CLI 部分已实现，完整产品路径未验收。标准为 [L1 v0.10 第 8 节](../../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 的 S5.A1、A2、A4、A5、A7–A13；本文件只提供路径，不复制另一套验收标准。

## 真实入口

客户端退出、服务停止/崩溃、重复投递、撤权、阻塞解决、未知资源核对与任务取消。

## 驾驶与证据

已受理 execute/rework 原样 retry 必须拒绝；新返工消耗额度。数字员工团队负责人失效时本人收到唯一待办，修复后仍由原成员继续。

Run 领取、单活动限制、重启 unknown、撤权不复活、取消等待停止、失败通知与状态原子提交已有核心测试。核心测试包含资源故障注入；另有真实 CLI/Keychain/Node 测试覆盖正常停服、强杀服务、旧进程组退出后恢复、失败不重派与不重复扣额度。真实 Docker 检查资源核对也有 Integration；正式恢复待办已有核心/CLI Integration，agent CLI 容器已接运行服务并通过协议替身的停止回收集成，原生模型的完整恢复仍待验收，不能据此标记本 Story 通过。

分别记录 Task、Delivery、Run、服务 epoch；已提交业务后终态丢失不得重复效果；无产出阻塞可在额度内继续。

CLI 和产品 Atelier Skill 分别验证；需要真实成员的路径不可用 stub 或宿主脚本替代。版本、对象和原始脱敏证据对应 L1/L2 v0.10。基础 CLI 的可用命令和集成测试见[实现记录](../../../../docs/implementation/1-first-team-delivery.md)；服务、成员执行与产品 Skill 路径尚未齐备，完整验收仍 BLOCKED，不填 pass。


## 正式阻塞的当前开发证据

成员 `task_report_blocker` 保存处理者、通知及停止请求；报告成功后 Run 仍占用名额。CLI `task blocker list/show/resolve` 与指定获准协调者 `blocker_resolve` 读取或记录修复依据，停止/unknown 未核对时拒绝解决，解决后也不自动运行。执行继续用显式 rework --blocker，检验继续用 verify --blocker；任务取消后旧阻塞保留为 superseded。空受控候选可以在资源确认停止后不生成 Artifact 而结束。

Integration 已覆盖无产出阻塞后的真实 CLI 解决与有限返工、数字协调成员解决/安排、事务失败回滚、取消和新旧版本；另有真实 Node 管道报告阻塞后接收停止信号并退出的检查。角色决定、环境修复依据与资源故障由测试构造，S5.A11/A12 尚无完整真实模型和产品 Skill 验收。retry 已有 CLI/受控成员入口；正式恢复选择/落实与 runtime reconcile 已提供，完整产品验收仍未完成。

## 显式投递重试

使用 `mailbox retry <投递> --revision <版本> --reason <依据>`，CLI 写操作带 requestId；数字负责人先从 `task_read.deliveries` 取本任务投递事实，再调用 `mailbox_retry`，可用 `message_respond(kind=retry, deliveryId=...)` 保存协调结果。原因预览截断会标记 reasonTruncated，不能将预览视作完整原记录。

覆盖：未运行的前置失败回 queued、已停止无终局的 coordinate/verify 保留原账本、新 Run 另计额度、承接/更新已提交后继续而不重复调用、已有终局不重跑、unknown/旧版本/撤权/额度耗尽拒绝、已受理 execute/rework 指向新返工。事务故障不得改变投递或保存不完整请求；重试不代成员选业务阶段。当前五个核心 Integration 含实际 CLI 重试与数字负责人受控工具证明；资源状态与成员决定仍为 fixture，完整真实模型/Skill 恢复链未验收。


## 本人恢复与离线资源核对

负责人配置前置失败、撤权或 Run 失败后，从本人 mailbox / task decision list/show 找到 kind=recovery；核对 Task、deliveryId、runId 和唯一事项。通过真实 CLI `task decision respond <ID> --revision <版本> --answer wait|retry|cancel` 保存选择；等待后可按新版本更新选择。`task recovery apply <ID> --revision <版本>` 实际落实，失败须看到 applied=false、responded 和持久 blocked_reason。选择 retry 不授权，不解除 unknown；选择 cancel 不直接关闭仍有活动资源的 Task。上述写操作需要 requestId。

服务未持锁时执行 `runtime reconcile`；存活进程组、缺 PID、未知归属容器均不得释放 Run，服务持锁时拒绝且不改变 epoch。资源确已停止后保存原效果，返回 stopped；有旧终局则 handled，无终局则 blocked，并保留本人恢复事项。核对不领取 queued、不增加 Run 额度；再根据本人选择显式 retry 或取消，负责人仍处理自己的工作。

当前开发检查覆盖真实 OS 进程存活/退出、服务锁、缺 PID、反复核对及终局待办闭合；真实 CLI/Keychain/Node 测试强杀服务后通过此入口核对实际子进程退出。模型配置是合成凭据与无效端点；它证明资源与事务接线，不证明 S5.A13 的真实模型恢复或产品 Skill。


## CLI 隔离资源的开发检查

`run show` 的 cliResources 展示登记的固定镜像、引擎身份、资源名称及核对状态；资源归属由核心在启动前保存。使用实际 `runtime reconcile` 验证：接入进程组存活/缺 PID、引擎不一致、同名异主容器或仍挂接其它容器的网络均保持 blocked_unknown，不误删、不释放 Run。原进程退出且全部归属资源移除后才 stopped；无业务终局的投递仍为 blocked，本人恢复待办保留，重复核对不消费额度。

开发入口：`cargo test --locked --test core real_docker_cli_reconcile -- --ignored --nocapture`，要求准备固定 Node 镜像。该测试运行真实 CLI、OS 进程及 Docker 容器/网络，容器中是有界合成进程，未运行 Grok/Pi；不能替代两种 agent CLI 的停止/续接与 S5 完整验收。归属异常和错误引擎记录为 fixture，Docker 观察、清理及持久结果为真实组件。

CLI 生产资源有创建许可：许可未授予的客户端中断可证明没有创建容器；许可授予后仍须核对进程组和归属资源。开发测试已覆盖登录代次变更拒绝、原子登记回滚及未授予许可时无需 Docker 的离线核对。旧记录缺许可字段继续保留原有缺 PID=unknown 规则。
