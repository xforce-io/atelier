# L2 技术设计：本机只读界面

版本：v0.1，2026-10-08；状态：Draft。关联 [Issue #25](https://github.com/xforce-io/atelier/issues/25)、[L1 v0.3](product.md)。

L1：write，v0.3，`docs/design/25-readonly-workspace-ui/product.md`。确认来源是 2026-10-08 本次会话对 v0.3 四点微调及沿用边界的确认。状态保持 Draft。

L2：write。依据是新增本机用户进程、只读数据访问和回环页面契约。本文不重复 L1 正文。

## 1. 设计依据与技术目标

落实 L1 v0.3 的 S1.A1、S2.A1、S3.A1、S3.A2、S4.A1、S5.A1。

技术约束：

- 第一方代码使用 safe Rust。
- 查看路径不得打开可写数据库连接，不得迁移 schema，不得创建 `runtime.lock`，不得调用 `runtime::start` 或 `runtime::stop`。
- 只监听 `127.0.0.1`。没有可配置的其它地址，也没有登录。
- 不读取 `credentials` 表，不展示凭据或签名材料。

非目标：新的消息列表命令、收件箱改版、会话模型、lockHeld 展示、条数上限、分页接口、GPUI。

## 2. 现状与改动范围

现有机制：

- 工作区数据库文件是 `<workspace>/atelier.sqlite3`（`src/store.rs` 的 `DATABASE`）。`Store::open` 以读写方式打开，并可能把 user_version 23 迁到 24。
- `worker list` 走 `Store::list("worker")`，按 `workers.rowid` 读出 worker JSON。字段含 `id`、`name`、`kind`。
- `runtime status`（`src/runtime.rs` 的 `status`）先 `Store::open`，再经 `lock_file` 创建或打开 `runtime.lock` 并探测共享锁，最后把库里的 state 叠成对外 state：锁被占用且库中是 `not_started` 时为 `starting`；锁被占用且已请求停止或库中是 `stopped` 时为 `stopping`；锁未被占用且库中是 `running` 时为 `interrupted`。没有 runtime 行时，快照里的 state 是 `not_started`。
- 工作消息写在 `messages`。`sender` 可空，仅系统入队不填发送者。`source = 'core'` 并不表示发送者为空：除 `work.note` 和 `work.question` 外，有发送者的消息也会标成 `core`。`recipient` 必有。插入投递时 `deliveries.receiver` 等于当时的 `recipient`；之后的更新只改投递状态，不改 receiver。没有列出「发送者或接收者」的命令。`mailbox list` 只按 `deliveries.receiver` 列出收到的，并带投递状态。
- 正文在 `messages.body`，非空。没有 `created_at`。先后是 `messages.rowid`。
- 没有本机页面，也没有回环查看服务。

改动：

- 新增只读查看模块和 CLI 子命令 `view`。
- 不改消息写入、收件箱、运行服务生命周期、凭据与签名。

## 3. 总体架构与关键路径

`view` 在进程内打开只读连接，绑定 `127.0.0.1:0`，向标准输出写出实际 URL，然后只回答 GET。页面和事实使用同一次只读查询结果。进程停在服务循环，直到中断。它不拉起 `runtime-serve`。

失败即停：

- 工作区不是已有数据库文件，或 user_version 不是 24：命令失败，不建目录，不迁移，不监听。
- 锁文件存在但不是普通文件：命令失败，与 `runtime status` 对损坏锁的拒绝一致。
- 请求的工作成员 id 不是该工作区的工作成员：该请求失败，不返回另一名的消息。
- 数据库在 2 秒忙等后仍不可读：该请求失败，不改用读写连接。

页面只有 GET。其它方法拒绝。正文以文本放入页面，不作为 HTML 执行。

## 4. 数据与状态契约

只读连接使用 `SQLITE_OPEN_READ_ONLY`，并设置 `query_only`。不调用 `Store::open`。不写 `user_version`。不读 `credentials`。

工作成员：`SELECT data FROM workers ORDER BY rowid`。页面使用其中的 `id`、`name`、`kind`。集合与 `worker list` 相同。

工作消息：点中 id 为 `W` 时

```sql
SELECT id, sender, recipient, body, rowid
FROM messages
WHERE sender = ?1 OR recipient = ?1
ORDER BY rowid ASC
```

接收者列只用 `messages.recipient`。不 JOIN `deliveries`。发送者 id 能对应工作成员时，带上该成员 id 和名称；`sender` 为空时，发送者标签为「核心」，不填工作成员 id。内容原样取 `body`。

运行状态：读取 `runtime` 单行；没有行则库内 state 为 `not_started`。锁探测打开已有的 `runtime.lock` 且不创建它。文件不存在则视为锁未被占用。叠字规则与 `src/runtime.rs` 的 `status` 相同，因此稳定时刻的 state 与 `runtime status` 的 state 相同。查看不插入 runtime 行。

写入次数：只读连接上的写语句会失败。验收用查看前后 `atelier.sqlite3` 的内容哈希，以及只读连接拒绝写入来证明写入次数为 0。

## 5. 接口与协作契约

命令：

```text
atelier --workspace <path> view
```

标准输出先写出 URL，然后进程保持监听，直到中断。`--json` 时这一行是既有 v2 封套，`data.url` 为 `http://127.0.0.1:<port>/`。否则标准输出一行这个 URL。只绑定 `127.0.0.1`。端口由系统分配。地址不可配置。

页面 `GET /`：

- 显示工作区 id、全部工作成员（id、名称）、运行 state。
- 未点成员时不出现工作消息。
- 点一名是打开 `GET /?worker=<id>#latest`。同一页列出该成员的消息：消息 id、发送者、接收者、内容。顺序从先到后。更后的一条带有页面锚点 `latest`，因此打开时可视范围落在更后的一端。
- 没有编辑、发送、成员管理、凭据、签名、启停控件，也没有对应的请求。未知查询参数直接失败。

`GET /facts` 返回页面所用的同一份只读事实：工作区 id、工作成员、state。`GET /workers/<id>/messages` 返回该成员的消息数组，顺序与上面的查询一致。未知 id 返回失败，正文不含其它成员的消息。这两个 GET 是页面的数据口，不构成第二条产品入口。

## 6. 运行与保障机制

- 监听地址固定 `127.0.0.1`。不接受 `0.0.0.0` 或其它地址的参数。
- 无登录、无会话 cookie、无写请求体。
- 忙等 2 秒，与 `Store::connect` 的 `busy_timeout` 相同。超时失败。
- 服务循环不调用运行服务的启动、停止或收件箱领取。
- 中断进程即停止监听。不改数据库。

## 7. 迁移、发布与回滚

不迁移现有数据库。user_version 不是 24 的工作区直接失败。回滚是不运行 `view`；已有命令和表结构不变。

## 8. 测试与验证

功能文件：`.agents/skills/verify-atelier/features/readonly-workspace-ui.md`。主要入口是真实 `view` 进程和它给出的本机页面。

| L1.8 | 机制 | 取证 |
|---|---|---|
| S1.A1 | `view` 指向临时工作区；另一次指向不存在的路径 | 页面上的工作区 id 等于该库；无效路径退出非 0，目录未创建 |
| S2.A1 | 页面工作成员与 `worker list` | 人数、id、名称一致；点中标记是被点的那一名 |
| S3.A1 | 页面消息与只读查询 | 含发出和收到；id、发送者、接收者、正文、rowid 顺序一致；空发送者显示「核心」；不出现投递状态；未点成员时页面无消息 |
| S3.A2 | 打开有多条消息的成员 | 更后一条在可视范围内；更早一条仍在同一列表中 |
| S4.A1 | 同一时刻页面 state 与 `runtime status` | 两边 state 字符串相同；查看前后 runtime 行不被 `view` 改写成 running |
| S5.A1 | 打开页面、点成员、请求事实 | 页面无五类入口；`atelier.sqlite3` 哈希不变；只读连接写语句失败；查询不涉及 `credentials` |

单元测试覆盖查询条件、空发送者标签、锁文件不存在时的叠字，以及只读连接拒绝写入。跨进程测试覆盖真实 `view` 与页面。不把单元测试当作 L1.8 通过。

## 9. 技术风险与开放问题

`runtime status` 会创建 `runtime.lock`，`view` 不创建。锁文件原本不存在时，两边都把锁视为未被占用，state 叠字结果相同。验收要在两边都能观察到的稳定时刻比较，不用一边创建锁文件的瞬间充当另一边的状态。

没有未决产品问题。命令名、回环绑定和「可视范围在更后一端」已在 L1 v0.3 写明。
