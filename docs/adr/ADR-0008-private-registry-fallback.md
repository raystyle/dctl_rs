# ADR-0008: 私仓直连与镜像拉取回落序

- 日期: 2026-09-21
- 状态: accepted
- 需求: REQ-005

## Context

三引擎的镜像拉取今天全部走 Docker 守护进程对 Docker Hub 的直连(bollard `/images/create`)。总台运行 registry.ohmygh.com(registry:2,v2 协议,凭据制);离线与受限网络场景下,Hub 不可达时 dctl 无路可走。REQ-005 要求:dctl 以原生 Rust HTTP 客户端直连私仓拉取常用数据库镜像,并定义离线回落序;准则 4 要求接入面与凭据边界先立本 ADR。

## Decision

1. **回落序(透明接入,不改命令面)**:引擎 `install`/`start` 的镜像获取统一走一条链:
   1. Docker 守护进程对 Docker Hub 拉取(现状路径,主选);
   2. 失败时 dctl 原生 v2 客户端直连 registry.ohmygh.com,取 manifest(含 manifest list 的平台选择)与 blobs,组装 **OCI image layout**,经 `/images/load` 灌入守护进程。
   3. 再失败时从本地缓存 tar(`~/.dctl/registry/cache/<slug>.tar`,由第 2 步成功时写入)同样灌入;成功的私仓拉取始终刷新缓存。
2. **显式命令**:`dctl local registry pull <image>[:tag]`(强制走私仓道并刷新缓存)与 `dctl local registry catalog`(列私仓 repos)。缓存管理即文件管理(删除即清),不做额外命令。
3. **凭据边界**:basic 凭据从本地密档 `~/.dctl/registry/auth` 读(`user:password` 一行,或裸 basic token);`DCTL_REGISTRY_AUTH` 环境变量作同格式携带者(金库/CI 注入,同 ledger 密钥的携带约定)。密档组/他读权限打警告(不硬拒,同 ledger pem);凭据零入仓、零 argv、零日志。
4. **端点**:`https://registry.ohmygh.com` 为常量;`DCTL_REGISTRY_URL` 为运维/测试覆盖旋钮(仅改端点,不改凭据语义)。
5. **格式**:OCI image layout(非 docker-save 私有格式):规范公开、`docker load` 原生接受、无需逆向 docker-save 的 manifest.json 方言;平台选择 linux/amd64|arm64 按 `std::env::consts::ARCH` 映射。
6. **与 omc 的关系**:纯 v2 协议语义复用,不耦合代码、不依赖 omc 二进制在端上存在;docker-load tar 通道即"端间转移兜底"。

## Consequences

- Hub 失败的拉取变慢(串行回落);缓存目录会增长,清理由用户按文件删。
- 新增一个需保护的本地密档(auth 文件),与 ledger pem 同等级对待。
- manifest list 与单 manifest、OCI 与 Docker 两族 media type 都要处理;blob 校验按 digest(sha256)进行,layout 天然按内容寻址。
- 预置 latest 锚清单与层遍历规范文档按 REQ-005 原文向总台提请,不阻塞本 ADR 的实现面。
