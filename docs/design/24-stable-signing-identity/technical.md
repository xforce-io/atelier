# L2 技术设计：重新编译后仍能读取已保存的 API 凭据

版本：v0.2，2026-10-11；状态：Draft。关联 [Issue #24](https://github.com/xforce-io/atelier/issues/24)、L1 [product.md](product.md) v0.3。验收 ID：S1.A1、S1.A2、S2.A1。

## 1. 设计依据与技术目标

L1 v0.3：API 凭据在工作区文件中。换二进制后读取同一文件。不使用钥匙串。

## 2. 现状与改动范围

写入、读取、删除都在 `src/credential.rs`。数据库仍只保存 `CredentialReference`。`connection test` 与运行服务预检调用 `load_secret(workspace, reference)`。

不再依赖 `security-framework`。不再把签名不匹配报成 `credential_unreadable`。文件缺失时，已有引用的读取返回「API 凭据缺失」；连接检查记为 `credential_unavailable`。

## 3. 总体架构与关键路径

核心进程按账号读取 `workspace/credentials/<account>`。账号形如 `工作区 ID:UUID`，不允许路径分隔。秘密仍经原有私有管道交给执行进程，不把凭据目录交给数字员工。

## 4. 数据与状态契约

一个连接版本对应一个账号、一个文件。文件模式为仅所有者可读写。符号链接视为无效，不跟随。

再次设置写成新账号的新文件。上一份文件保留，原请求仍能按原秘密重放。清除只删除该次操作记下的账号文件。数据库与请求指纹不含秘密。

## 5. 接口与协作契约

`connection credential set`、`connection test`、`runtime start` 的参数不变。命令输出不含秘密。

## 6. 运行与保障机制

读取不弹窗，也不访问钥匙串。工作区目录被复制时，凭据文件随之复制。

## 7. 迁移、发布与回滚

没有从钥匙串到文件的迁移。已有钥匙串项留在原处，不被读取；本人再设置一次后写入文件。

## 8. 测试与验证

功能地图：[setup-and-intake.md](../../../.agents/skills/verify-atelier/features/setup-and-intake.md)。

| 验收 | 机制 |
|---|---|
| S1.A1 | `tests/cli.rs` 的 `rebuilt_binary_reads_the_workspace_credential_file`：两份二进制副本，第二份 `connection test` 不再因凭据失败 |
| S1.A2 | 运行服务预检读取同一文件。本轮核心与 CLI 证明文件可读；服务领取路径随既有运行服务测试，未单独新开钥匙串环境 |
| S2.A1 | 有引用而无文件时读取失败，且代码路径不调用钥匙串 |

## 9. 技术风险与开放问题

无开放问题。
