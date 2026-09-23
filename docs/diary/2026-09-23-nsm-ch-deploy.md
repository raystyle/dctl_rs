# NSM CH 面落地:--bind 批与 lan-linux 部署

- 日期: 2026-09-23
- 批面:REQ-012 LAN 面发布旗标 + 总台 NSM 执行单(起实例、建库、发号)

## --bind 批(REQ-012)

- 动因:总台 NSM 单要传感器(192.168.88.4)直连 lan-linux(192.168.88.175)上 dctl 管理的 CH 写入,而 `create_clickhouse` 端口发布硬编码 `host_ip: 127.0.0.1`。无旁路,立 REQ 走码批。
- 一轮 81b3d03:旗标 + 面感知端口检查 + 四分支测试。评审回执 F1/F2 必修、G1-G4 建议。
- F1(评审真机实证的深坑):`::` 走"通配替换"分支会丢 loopback,宿主就绪探针打 127.0.0.1 永远超时。根因是**两族通配不同质**:Docker 发布 `[::]` 是 v6-only(v4 loopback 不可达),同口 `[::]`+`127.0.0.1` 双绑可共存;`0.0.0.0` 才覆盖 loopback。修法 (b) 按族特判:v4 通配单面,`::` 走默认分支带 loopback 伴面。
- G1(同源实证):非本机面探针 EADDRNOTAVAIL 被折算成"口被占",自动挑选退化成"找不到空闲口"。修:validate_start_options 面存在性预检 + 专属文案「not present on this host」。含义:**dctl 须在宿主 netns 运行**(容器里跑 dctl 打宿主 IP 面即触发)。
- 修订 f7a39d2,二轮快核 CONFIRM 放行(评审者复跑四闸门 + 真机四面:定点/0.0.0.0/::/192.0.2.1 专属报错)。
- 评审格曾被回收(pitfalls 在册病例),重驻 = 右分新格 + agent start 沿用 live 名 dctl-codex-review。

## lan-linux 部署(NSM 执行单)

- 形态:rsync 仓到 /home/ray/dctl_rs(持久 ops cwd),rust:1-slim 构建,二进制落 /home/ray/.local/bin/dctl,**宿主直跑**(netns 前提 + ray 在 docker 组)。
- 实例:`dctl clickhouse start nsm --version 26.8 --http-port 8123 --native-port 9000 --bind 192.168.88.175`;容器 dctl-ch-nsm-26.8,ss 双面四口,docker ps 见 127.0.0.1+192.168.88.175 双绑定;loopback/LAN/WSL 异机三点 /ping 全 Ok.。
- 建库 nsm:五 raw 表(flows/dns/tls/alerts/eve_other,MergeTree,PARTITION BY toYYYYMM(ts),排序键时间桶在前)+ AMT 五面(flows_daily/dns_daily/tls_sni_daily/tls_ja4_daily/alerts_daily,AggregatingMergeTree + 五 MV)。总台契约列名类型为唯一真源;`start`/`end` 列名反引号。契约写"四物化视图"但枚举五面,按枚举建五。
- 账号:实例以 `-e CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT=1` 起(default 用户 SQL 管理权,首建实例忘了此 env 会在 CREATE USER 处 497,重建补);vector 专用号 insert-only,口令 openssl rand -hex 16 存 /home/ray/.vector_creds(0600)。

## 踩坑记档

- **CH 物化视图在插入者权限下执行**:仅 GRANT INSERT 不够,MV 推送要求源表列级 SELECT。解法 = 列级最小授权(MV 读到的列,不含 raw):flows(ts,src_ip,bytes*,pkts*)/dns(ts,src_ip,rrname)/tls(ts,src_ip,sni,ja4)/alerts(ts,signature)。冒烟实证:vector 写 flows 200,MV 面出 (day,src_ip,100,200);读 raw 403(列级钉住,写号不能回读原文)。
- **持久 cargo 缓存卷盖掉镜像自带 cargo**:空 bind mount 挂 /usr/local/cargo 后 `exec: "cargo": not found`(named volume 首挂自动拷镜像内容,bind mount 不会)。先播种:`docker run --rm -v /tmp/dctl_cargo:/mnt rust:1-slim sh -c "cp -a /usr/local/cargo/. /mnt/"`。
- toYYYYMM(DateTime64) 的 DROP PARTITION 用 `DROP PARTITION ID '200001'`(裸数字 200001 报「Wrong number of fields」)。
- 验收冒烟法:vector 写一行 ts=2000-01-01 的合成流(独立分区),验 MV 链后 DROP PARTITION ID 清除,零残留不污染对账窗。

## skills 剪枝批(REQ-013,同日晚)

- 动因:用户令修正对齐 skill 面。上游 agent-skills 全量 11 项里,infra-* 教上游 clickhousectl 命令加 ClickHouse Cloud 编排、managed-postgres-rca 走 api.clickhouse.cloud 云 API、chdb-*/clickhouse-js-node-*/clickstack 均域外;保留纯引擎知识两项(architecture-advisor、best-practices)。
- 落法:RETAINED_SKILLS 收集过滤 + 装后 prune(目录名 ∈ 档案 slug 集 且 ∉ 保留集 才清;用户自有技能不动;目录缺失 no-op);JSON 按 agent 增 pruned_skills。
- 评审 G 处置:G1 本节补档、G2 根帮助口径同步 curated、G4 两处 prune 边界记 REQ(同名误清、下架项不清)、G5 认正(check-md 实为 40 文件)。码面 CONFIRM,实机冒烟(2 项列表 + 五目录清理)后推。
