# 井字棋可信检查资源

检查脚本和 npm 锁文件属于可信检查资源，独立于 `samples/tic-tac-toe` 候选目录。采用 Chromium 单进程模式控制线程开销，缓存写入受限 /tmp；只在隔离检查镜像中读取只读候选，不在宿主执行候选脚本。候选接口约定见样例 README；正常任务承接前需固定该配置和镜像摘要。

`check.mjs` 在断网浏览器中直接打开候选首页，执行 19 个场景：初始/轮次/重复点击，X 和 O 各八条胜利线及结束后不变，平局/重开，游戏中重开和连续新局。输出带首页 SHA-256 的 JSON；任一场景失败退出 1，检查器启动失败也不能当通过。完整 Artifact 摘要须由核心另外固定和关联，首页摘要不代替整个产出版本。

准备镜像时先取得 `mcr.microsoft.com/playwright:v1.56.1-noble` 的 RepoDigest，将实际 `mcr.microsoft.com/playwright@sha256:…` 作为 `BASE_IMAGE` 构建参数；不要把可变 tag 作为冻结配置。Dockerfile 中的 npm 安装只用于可信镜像构建，运行检查时禁止联网。镜像构建后使用实际 `sha256:…` Image ID 运行，不猜测摘要。

```sh
docker pull mcr.microsoft.com/playwright:v1.56.1-noble
docker image inspect mcr.microsoft.com/playwright:v1.56.1-noble --format '{{json .RepoDigests}}'
# 将上一步实际 RepoDigest 传给 --build-arg BASE_IMAGE=...
docker build --build-arg BASE_IMAGE=<实际RepoDigest> -t atelier-tic-tac-toe-check:v1 .
docker image inspect atelier-tic-tac-toe-check:v1 --format '{{.Id}}'
# 将上一步实际 Image ID 传给校准脚本
node calibrate.mjs <实际ImageID>
```

`calibrate.mjs` 固定使用无网络、只读根、非 root 用户、无额外 capability、禁止提权、1 CPU、512 MiB、64 进程及 120 秒上限。候选只读挂载，仅 /tmp 可写；不挂载核心记录、凭据或宿主管理 socket。检查超时会清理本次具名容器。若限制下无法运行，报告阻塞，不放宽限制。

校准分别检查已知缺陷基线和临时修正的参考文件：前者应恰有四个对角线场景失败，后者应全通过。临时参考只验证检查器判定能力，不是成员交付，不能用来通过 S3 的真实失败返工或任何产品验收。容器退出后删除临时目录。
