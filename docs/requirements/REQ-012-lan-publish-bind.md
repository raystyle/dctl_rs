---
id: REQ-012
title: clickhouse start 增 --bind 旗标,端口可发布到非 loopback 面
status: draft
priority: must
trace: null
---

# clickhouse start 增 --bind 旗标

## Scenario

总台派单(2026-09-23,NSM 留存库批):lan-linux(192.168.88.175)上 dctl 管理的 ClickHouse 实例须接受传感器(192.168.88.4)直连写入,而 `create_clickhouse` 的端口发布硬编码 `host_ip: 127.0.0.1`,loopback 面外不可达。需要显式 opt-in 通道把 8123/9000 发布到指定主机面。

## Criteria

- [ ] `server start`(即 `dctl clickhouse start`)新增 `--bind <IP>`:默认 loopback 发布之外追加一个发布面;`0.0.0.0`(v4 通配)替换 loopback(v4 通配已覆盖之,同口通配与定点绑定冲突);`::`(v6 通配)追加 v6 面并保留 loopback(Docker 的 v6 发布不服务 v4,同口双绑可共存);face 为 127.0.0.1 时去重为单面
- [ ] 不带 `--bind` 行为零变化(仅 loopback,默认安全面不动)
- [ ] 值必须是可解析 IP 地址(v4/v6 皆收,规范化存储),clap 层拒收非 IP
- [ ] 端口可用性检查与自动挑选覆盖绑定面:显式口在任一面被占即报 PortInUse,自动挑选避开任一面被占的口;绑定面不在本 netns 时报专属文案(不再折算为口被占)
- [ ] resume 语义与端口旗标一致:`--bind` 在 resume 时忽略(容器保持既有绑定)
- [ ] 帮助文本、README、CONTEXT FOR AGENTS 注记同步
- [ ] 测试:clap 解析正反例、面感知端口检查单测、假 Docker create 体的 HostIp 断言(定点/0.0.0.0/127.0.0.1/:: 四分支)

## 非目标

- Postgres/FalkorDB 对称旗标:本批只做有直连需求的 ClickHouse 面,对称面待需求出现另立单
- 服务器 TLS 与 mTLS:ADR-0011 后续批另行推进
- 发布面的防火墙收口:属部署侧义务,不进本仓

## 验收判据

- 双 clippy 零警告、fmt 门禁过、全量测试绿(计数随新增测试增加)
- dctl 须在宿主 netns 运行:非本机面会因探针 EADDRNOTAVAIL 报「not present on this host」,容器内跑 dctl 即此例
- lan-linux 实机:`--bind 192.168.88.175` 起实例,ss -tln 见 127.0.0.1 与 192.168.88.175 双面监听,异机 curl 8123/ping 通
