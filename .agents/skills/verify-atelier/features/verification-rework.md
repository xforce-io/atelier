# S3 验证路径

状态：交接基础路径已实现，真实检验与返工未验收。标准为 [L1 v0.10 第 8 节](../../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 的 S3.A1–A7；本文件只提供路径，不复制另一套验收标准。

## 真实入口

执行者获准直接交接或团队负责人安排；检验者接收/拒收、失败、团队负责人有限返工、新版重验。

## 驾驶与证据

交接结果由成员真实工具调用产生；记录旧问题、消息、原生上下文和独立检查证据；检验重试计总预算。

CLI 和产品 Atelier Skill 分别验证；需要真实成员的路径不可用 stub 或宿主脚本替代。版本、对象和原始脱敏证据对应 L1/L2 v0.10。当前 CLI task verify/handoff show 与成员交接工具已有集成测试，核心检查已接真实 Docker；返工 CLI/成员安排已实现，真实模型团队路径尚未打通，BLOCKED，不填 pass。


## 当前实现证据边界

五个核心交接集成测试覆盖执行者授权直交意图、负责人真实 CLI 送检、拒收通知与补充后重交接、接受不能冒充检验、重复/范围/partial 校验、预算及存储失败。产出固定前无检验投递，无意图则不自动送检。

核心检查集成另通过真实 Docker 运行缺陷候选和测试参考候选，分别得到 15 通过/4 失败与 19 通过；模型 fixture 对失败证据建议 pass 仍被核心记为 fail。同一检查重放不重启容器。实际检查隔离参数和原始日志均保存；服务重启恢复测试确认归属不符的容器不清理、归属匹配后结果为 inconclusive、其他 Run 资源仍 unknown。这些测试的候选和成员决定由测试构造，不替代 S3.A1–A7 的真实团队证据。具体命令与证据见实现记录。


执行者自测已有核心与真实 Docker Integration：旧候选检查请求保存后修复并重开数据库，旧检查仍失败、新检查通过；结果绑定各自候选版本和摘要，不产生独立检验、交接或验收。自测成功不能提交 verification，自测失败也不计作 S3.A1 独立检验失败。真实成员自测与独立检验的完整路径仍待验收。


返工入口：有权人类团队负责人使用 `task rework --verification <失败检验 ID>`，已核对的执行失败使用互斥的 `--failed-run <Run ID>`；数字团队负责人使用 `task_arrange` 的 rework 和原因引用。检查 `task show` 的预留/消费记录，覆盖重复请求、重启、2 次上限、未领取取消释放、撤权 blocked、旧原因和旧检查拒绝。新增真实 CLI/Docker Integration 已跑通缺陷独立检验失败→明确返工→新产出→独立检查通过；决定与代码修复仍是 fixture，不作为 S3.A1/S3.A5/S7 的真实成员验收。阻塞解决原因路径已有 Integration，验收拒绝可使用 `--rejection`；核心及真实 Docker/CLI 已覆盖拒绝后的新版本和新检验，仍不作为真实成员验收。


检验成员报告阻塞可以发生在接收前或接收后；资源停止并正式解决后，团队负责人用 `task verify ... --blocker <ID>` 新建交接。旧交接保留 superseded 状态，新投递重新接收并计总 Run 额度，代码返工计数不变；重复决定和旧 blocker 不能产生第二个新交接。核心故障注入覆盖旧交接替代与新消息同事务回滚，未替代真实成员路径证明。

已终局的 inconclusive 使用 `task verify ... --inconclusive <Verification ID>`，与 `--blocker` 互斥；数字负责人使用 task_arrange 的 verificationId。检查旧交接 superseded 且保留原接收理由/替代依据、新投递须重新接收、新检查 ID/Run 与旧记录不同、总 Run 额度增加而代码返工不增加。明确 pass/fail、旧检验、未停止资源、重复安排均拒绝；新通知写入失败时旧交接替代一并回滚。核心与真实 Docker/CLI Integration 已覆盖，但数字决定仍为 fixture，不冒充真实团队验收。

2026-10-03 真实失败返工证据：任务 `e817a65c…`（Grok 执行/Pi 检验）与 `54460c41…`（Pi 执行/Grok 检验）在创建时明确先原样提交遗留基线检查；各自独立检查真实得到 15 pass / 4 fail，负责人据对应失败检验安排一次返工，新固定版本独立检查 19/19，随后保存各自人类待办。两种 CLI 的执行 context 均跨 Run 保留且返工 resume=true；各自独立检验 context 也跨两次检查续接，与执行 context 不同。操作、因果、原始检查、版本及导出摘要见 `baseline-review-grok-result-20261003.json`、`baseline-review-pi-result-20261003.json`。这是显式基线评审场景，不把初始基线称为合格交付；当前人类试玩/接受及其余异常分支仍未完成。

2026-10-04：真实 Pi/Grok 演练 `4f4c5684…` 完成资料不齐拒收、同产出补齐后新交接；发现冻结 checkId 不可见后由成员正式报阻塞。修复 `615e884` 后原负责人自行核对并解决阻塞，原检验成员新交接后19/19。S3.A3/A6 开发证据齐备；旧 rejected/superseded、当前 accepted 与所有停止事实保留，尚无当前人类接受。见 `skill-handoff-rejection-result-20261004.json`。

L2 v0.23：负责人从 task_read.checks 或检验结果通知的 checkRecordId 取得 check_read.id；冻结 verificationProfile.checkId 仅是 run_check 的名称。真实空输入恢复暴露负责人猜测检查 ID 的缺口，新增索引后用核心断言验证负责人可读本任务、其他成员不可见别人的检查；真实成员复核待补。不得将最终提出验收请求当作检查证据查询已成功。

`a272a31` 修复后的实际 Grok 负责人已通过当前 task_read.checks 发现记录并成功 check_read，4 次工具调用、零错误，核对原独立检查 19/19；原验收请求和任务不变。见 `check-index-proof-result-20261004.json`，服务已停止；该可发现性缺口已复核。


2026-10-04：S3.A4 显式运行真实 Docker：含未完成项、报告后异常退出和报告后耗尽120秒时限，均 inconclusive 且资源停止；部分内容不产生独立通过，另有 inconclusive 验收请求拒绝断言。证据 check-inconclusive-f2c9b43f-86a5-437a-bcf9-4a7f33f2f6a4.json。成员及可信检查为 fixture，未冒充真实模型验收。
