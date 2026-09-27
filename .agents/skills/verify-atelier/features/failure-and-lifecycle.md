# S5 验证路径

状态：待实现、未执行。标准为 [L1 v0.4 第 8 节](../../../../docs/design/1-first-team-delivery/product.md#8-验收与效果验证) 的 S5.A1、A2、A4、A5、A7–A9；不复制另一套成功标准。

## 真实入口

CLI cancel/reconcile/request show、信号；Skill 超时与重入。

## 驾驶与证据

三类失败、Ctrl-C/强杀/未知、并发取消、持久化失败、文本/JSON；超时先查不重放。

按 L1 每项前置、操作、禁止结果与证据要求执行，关联同一候选和设计版本；实际命令/宿主配置未落实前 BLOCKED。CLI 与 Skill 记录分开，不能以安装文本或直接调用 API 代替对应入口。
