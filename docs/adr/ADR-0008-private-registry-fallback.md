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

> 追注(2026-09-21,用户令经总台转):决策 1 第 2 步的直连腿由自研原生 v2 HTTP 客户端**换轨为官方生态库 oci-client**(github.com/oras-project/rust-oci-client,crates.io 名 oci-client,v0.18);自研腿收敛为库封装(manifest 原始字节、平台选择、blob 流式与 digest 校验、basic 挑战式鉴权均由库承担,OCI layout 组装与缓存仍在本仓)。背景:总台把 registry.ohmygh.com 切 R2 Worker 只读面(ohmycloud 仓 ADR-0002 加 REQ-064,实施中),对外 v2 协议面不变(GET 加 HEAD),basic 凭据与本地密档供给道不变,写入面恒 405;回落链序与本 ADR 其余决策不变。REQ-005 验收面不动。
>
> **服务端契约(定口径 c9c7fda9,2026-09-21 晚用户令)**:「匿名可拉取,不可枚举」:`/v2/` ping、manifests、blobs 匿名 200,oci-client 零凭据走完拉取链;`/v2/_catalog` 与 tags/list 恒 401 加 `WWW-Authenticate: Basic` 挑战(错凭据同挑战)。库的 Basic 是挑战门控且仅在 `/v2/` 探测点触发,匿名 200 面下凭据不会随库请求携带,故**枚举保留一处自建面**:`catalog` 命令走预带 Basic 头的显式请求(换轨令边界本为拉 manifest 加 blob,枚举不属换轨面,此例外记档);dctl 不做 tag 发现(引擎版本锚为固定引用,latest 锚清单即总台此用途),回落链按 ref 拉取零影响。沿革:439402f8 为匿名读中间态(此前全挑战式,与本仓 stub 行为同形),终口径 c9c7fda9。
>
> **库鉴权语义免疫记录(2026-09-21 深夜,用户口径核验)**:oci-client 的 `store_auth_if_needed` 首存即定格(同 Client 二次存不同凭据被静默丢弃),升级鉴权须每端点新建 Client;匿名在 bearer 域非无 token。本仓四条皆结构性免疫:每进程单端点、凭据构造时一次定格、Hub 腿走守护进程(bollard)不经此 Client、本面为 Basic/无挑战不触发 token 流。若未来多端点或运行中换凭据,须按端点/凭据对各建 Client(语义注记同步在 registry.rs 结构体注释)。

## Consequences

- Hub 失败的拉取变慢(串行回落);缓存目录会增长,清理由用户按文件删。
- 新增一个需保护的本地密档(auth 文件),与 ledger pem 同等级对待。
- manifest list 与单 manifest、OCI 与 Docker 两族 media type 都要处理;blob 校验按 digest(sha256)进行,layout 天然按内容寻址。
- 预置 latest 锚清单与层遍历规范文档按 REQ-005 原文向总台提请,不阻塞本 ADR 的实现面。
