# registry-fallback 批:评审闸门轮实录与真机踩坑

- 日期: 2026-09-21
- 批面:ADR-0008 私仓直连与回落序(REQ-005),工作区批基线 2e8f817

## 评审轮次(dctl-codex-review,tab 右格)

- 一轮全量:回执 F1-F4 必修、G1-G7 建议、不 CONFIRM。
- 修订:F/G 全处置;G1 反证纠正评审方(第三子句非死逻辑,`db/tools:latest` 靠它切分)。
- 二轮快核:CONFIRM 放行;另留两条非阻塞备注(diary 记档、endpoint 旋钮 userinfo 防呆),均采纳落实。

## 派单前自查修掉的五处(评审请求已列)

1. 缓存刷新从不生效:`fs::copy` 目标父目录未建,ENOENT 被 `let _` 吞;改 `create_dir_all` 后 copy,失败可见告警。
2. `pack_layout_tar` 把正在写的 `image.tar` 打进包(体积近翻倍);改跳过自身,测试断言无自打包条目。
3. manifest blob 曾用 serde 重序列化字节:registry:2 的 Go 字段序与 serde 字母序不同会破坏内容寻址;改保留原始字节、digest 从交付字节计算。
4. `reference_splitting_shapes` 空壳测试(循环体 `let _` 不断言):删改 `registry.rs` 内真单测。
5. carrier 凭据测试名不副实:stub 改录 Authorization 头真断言。

## 真机踩坑:containerd 镜像存储的字面名(最重要)

lan-linux(Docker 29.8.1,containerd 镜像存储)真 registry:2 + 真 daemon 端到端抓出:`docker load` 按 OCI layout 的 `ref.name` 注记**字面**起名(如 `postgres:18`),而引用解析按归一名(`docker.io/library/postgres:18`)匹配,结果 `docker images` 里在列、`docker run` 却解析不到、转投 Hub。classic 存储时代 load 会归一化,此坑为 containerd 存储特有。修复:index.json 注记写全限定名(`fully_qualified`:单段名进 `docker.io/library/`、多段进 `docker.io/`、首段带点或冒号或 localhost 视为 host 原样)。复证:`docker run --pull=never` 直跑、inspect 解析到灌入镜像、ctr 见 fq 名在册。

## lan-linux 配方复用与播种

- 沿用 2026-09-20-fork-bootstrap 的 rust:1-slim 配方(socket gid 983,`--network host` 使 127.0.0.1:5000 可达);`--features telemetry` 已随 telemetry 退役删除。
- 播种:registry:2 起临时容器,三引擎最新稳定版 Hub 原名推入(postgres:18、clickhouse/clickhouse-server:latest、falkordb/falkordb:latest);拉取前删本地 tag 保证 load 可观测。
- 容器卷持久化(/tmp/dctl_cargo、/tmp/dctl_target)加速复轮。
- 实证面:两轮全量测试绿、三镜像原生 v2 拉取、缓存 tar 落盘、引擎级生命周期(start 到 client 查询到 stop 到 remove)全通;falkordb 走 Docker schema2、postgres/clickhouse 走 OCI manifest,两族 media type 实弹都过。

## 遗留(记档不做或后续)

- docker_load 整 tar 入内存 Vec:GB 级镜像需流式化,后续批。(已随同日 pull-streaming 批完成,见下节)
- 私仓 repo 命名(Hub 原名还是别名托管)与 latest 锚清单:归总台裁定,REQ-005 追注在册。(同日下午已裁定:原名托管零映射、建清单机制,见上文总台三件回执节;余下仅补种执行)
- lan-linux 上本轮残留:dctl-registry-test 容器(含播种数据)、三个已灌镜像与裸名别名、/tmp/dctl_cargo 与 /tmp/dctl_target 缓存卷;复验后按需清理。(终审后已全清)

## 总台三件回执与 write-face 收口(同日下午,飞轮派单)

- 经 herdr 派单 ohmycloud 工位:件 1 registry 命名裁定为 Hub 原名托管零映射(现存六仓均为原样先例;三引擎尚未托管,态缺,补种道 omc dist images push 经 tc-bj 中转,等总台令);件 2 裁定建 latest 锚清单机制(草案 postgres:18、falkordb/falkordb:v4.20.6、clickhouse/clickhouse-server:26.8,平台随上游 multi-arch,落点 catalog 侧镜像锚节);件 3 kid 注册完成(D1 pubkeys 直插,active,与 keys.rs 逐字一致)。
- 本侧断言:keys.rs 常量对账一致;write-face 复测 `dctl ledger issue new` 冒烟即 issue #1 注册成功、401 消失,REQ-006 写面判据收口。

## oci-client 换轨批与 R2 面专测轮(同日晚间)

- 换轨批(3801d2e,总台转用户令):直连腿换 oci-client 0.18,评审 CONFIRM 零 F 项;真机三 digest 与自研腿逐字节一致;ADR-0008 追注换轨记录。
- R2 面(registry.ohmygh.com Worker 化)当晚上线,口径两跳:439402f8 为匿名读中间态(此前全挑战式),终口径 c9c7fda9「匿名可拉取,不可枚举」(ping/manifests/blobs 匿名 200;catalog 与 tags/list 恒 401 加 Basic 挑战,错凭据同挑战)。本侧三面实证吻合。
- 专测轮:三引擎生产面匿名拉取全过,postgres:18 digest 与 Hub 侧逐字节一致(0377e72c),两个 latest(bc33a0f4/13197149)属移动 tag 上游漂移,拉取与运行正常;回落链第二级实测(断 hosts 断 Hub)教科书级通过:Hub 失败原因通报、R2 回落、install 成功、链灌镜像 --pull=never 可运行。
- ledger 台账清零(2026-09-22):issue #1(write-face 复测冒烟)由总台关单(ledger seq 193,断言 = REQ-006 第 38 行收口节 + seq 175 kid 签名在册事实)。本仓 ledger 无挂起单。
- 契约文档轮评审 F1(评审方抓的深坑):oci-client 挑战门控仅在 /v2/ 探测点触发,匿名 200 面下凭据永不随库请求携带,catalog 走库必 401,落密档也不解,「枚举须携凭据」高估了库能力。处置选代码侧:catalog 保留一处自建预带 Basic 的显式请求(换轨令边界本为拉 manifest 加 blob,枚举不在列,例外记档 ADR-0008);无凭据时报边界错且零外发请求(结构判别负例钉住)。真面枚举验证待密档落位。
- 用户注记收录(oci-client 实操语义,registry.rs 结构体上有同文注释):真裸访问须自建无鉴权仓(Anonymous 直行不带 Authorization,公网 Hub/GHCR 通常 401);`store_auth_if_needed` 只存第一次凭据,同 Client 二次 store 被丢,升级鉴权须新建 Client;按仓分流(公开 Anonymous、GHCR 私有 Basic 空用户名加 token 或 Bearer,同进程不同仓用不同 Client);匿名不等于无 token,bearer 域匿名 token 仍随后续请求。本仓四条皆结构性安全:单端点单凭据构造时定格、Hub 腿走守护进程不经此 Client、面为 Basic/无挑战。
- 契约第四跳(深夜用户令 7741e5e7):枚举面同开匿名,终态 = 读面全匿名(拉取加清单)、写恒 405、错凭据仍 401 加挑战。挂起的真面枚举验证随之解除(匿名即通)。处置:catalog 自建面保留但改为两态稳,即有凭据预带 Basic(两面都被接受),无凭据匿名直行;不走纯库原生(挑战门控在枚举再收口时会静默 401)。无凭据负例翻转为匿名直行正例(前条 F1 的「无凭据报边界错零外发」已随本跳翻转为匿名直行);ADR 契约段按终态重写,README 去「需凭据」注。评审 G1 采纳:预带凭据遇 401 时匿名重打一次,陈旧密档不挡开放面的枚举。

## 流式化批(pull-streaming,同日第二批)

registry-fallback 批 G5 记档的内存面收口,行为零变化:

- blob_to:层 blob 从 response.bytes() 整层缓冲改为 bytes_stream 分块,边下载边 sha256 边落盘,收尾比对 digest;不匹配时 partial 文件只存在于 staging tempdir 内随拉取失败回收。
- docker_load:tar 从整包 read_to_end 内存 Vec 改为 tokio File 按块(512KiB)futures unfold 流式读,bollard::body_try_stream 喂 /images/load;内存峰值与镜像尺寸脱钩。
- 依赖面:bytes 提为直依赖(仅为命名 Bytes 类型,传递树里本来就有,零新增编译面)。
- 验证:本地全量 309 例全绿(单测 215 加集成 94,退出码实证);lan-linux 真 registry:2 三引擎复验与峰值 RSS 实测见当轮评审请求。
