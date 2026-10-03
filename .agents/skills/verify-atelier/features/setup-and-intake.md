# S1 验证路径

状态：基础 CLI 部分已实现，完整产品路径未验收。标准为 [L1 v0.10 第 8 节](../../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 的 S1.A1–A3、A7–A17；本文件只提供路径，不复制另一套验收标准。

S1.A16 的准备入口现为 `connection prepare <id> --revision <n> --worker <id> [--version <冻结版本>]`，带稳定 requestId；目前核对已显式准备的本地固定镜像并建立 Worker 私有存储。记录 preparing/prepared/failed 与固定错误类别，经 connection show/request show 复查，prepared 不等于登录或执行可用。镜像构建开发脚本不能替代完整产品准备路径；专用登录、检查、真实运行与续接尚未贯通，S1.A16 不记 pass。分别验证两个 Worker 不共用目录、重复请求不重做、已准备目录丢失不补建、没有 Task/Run 副作用。

## 真实入口

`connection login` 已提供管理入口，参数与 prepare 对应；首次新 requestId 需要交互终端且不带 `--json`。已有请求可非交互重放或核对，不能重新认证；connection show 返回 cliLogins 与登录代次。开发集成已用实际 CLI、PTY、Docker 和两种登录替身验证成功材料保存、取消回收、代次失效和没有业务工作副作用；尚无原生账号认证或模型成功证明。完整 S1.A16 仍未通过。

初始化、配置本人及三名数字员工、指定人或数字员工团队负责人、授权、隔离 CLI 准备/登录和运行服务启动。

## 驾驶与证据

当前不可变连接版本、Worker 显式绑定及 Task 冻结快照已有 CLI/核心集成测试；API 凭据和接入进程已用真实 CLI/Keychain/Node 验证；连接检查已有入口与失败路径测试，Skill 安装/描述及当前宿主基础组队已有开发观察，真实模型成功与隔离 CLI 尚未贯通。

关联身份、授权、Skill 加载摘要、镜像、登录与服务 epoch；验证启动终端退出后服务独立存活。

CLI 和产品 Atelier Skill 分别验证；需要真实成员的路径不可用 stub 或宿主脚本替代。版本、对象和原始脱敏证据对应 L1/L2 v0.10。基础 CLI 的可用命令和集成测试见[实现记录](../../../../docs/implementation/1-first-team-delivery.md)；服务、成员执行与产品 Skill 路径尚未齐备，完整验收仍 BLOCKED，不填 pass。


API 凭据现有真实 Keychain 与 CLI 测试：stdin 输入、无明文回显/落库、按连接版本绑定、重放/换秘密冲突、轮换和显式修复旧冻结版本、本地清除及失败重试。Keychain 读取/清理由创建者程序执行，未放宽系统访问控制；全部使用合成秘密。connection test 已支持 API 的最小无工具请求、30 秒有界观察/回收、失败脱敏与原请求重放；connection show 核对版本、凭据代次和接入版本。工作说明修改不使检查失效，修复后的新代次须用新 requestId 重测；成功结果的绑定测试使用 fixture。真实端点成功、两种隔离 CLI 登录/检查及产品 Skill 的完整真实团队路径未完成，不能据此汇总 S1 通过。


## 产品 Skill 安装与范围

在专用临时目录执行 `skill install --destination <目录>`，记录入口摘要，让实际宿主读取该入口并按其指引调用 `skill describe --protocol 2`。工作区不存在显示初始化，损坏工作区或协议不兼容报错；安装相同内容可重复，修改后的文件和链接目标不得覆盖。再从实际入口建立本人/三名数字员工、团队和授权；切换到 --team / --task 后记录真实身份、当前授权版本、冻结职责和操作索引。本机本人有 arrange 授权但非冻结负责人时不得出现成员协调操作；撤销验收权后不得提供接受/拒绝调用路径。

成员 Run 的工具与操作文件一一对应，输入 schema 一致；执行成员不获得负责人/检验完整路径，检验者不获得写文件工具。当前 Integration 已验证这些边界；宿主开发观察只完成空目录组队和 pending 投递，未使用真实模型、未证明宿主退出后的团队推进，不能汇总 S1/S7 为 pass。
