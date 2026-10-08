---
id: REQ-0017
title: FalkorDB 与 ClickHouse 的 mTLS 腿(ADR-0011 收官;FK 先过实测闸门)
status: done
priority: should
trace: 立项于 2026-10-08(REQ-0016 PG 腿落地后);判据承 REQ-009「FK/CH 后续批」,设计真源 ADR-0011
---

# FalkorDB 与 ClickHouse 的 mTLS 腿

## Scenario

ADR-0011(accepted)裁定三引擎免密 mTLS;PG 腿已闭环(REQ-0016:证书面默认 + 口令回落,实弹 11/11)。本 REQ 收官 FK/CH 两腿。ADR-0011 决策 5 明令 FK 的最大不确定项留**实测闸门**:基座 redis 的 TLS/ACL 语义(`tls-auth-clients-user` 需 redis 8.6+?镜像是否 BUILD_TLS?)未证;不支持则按 ADR 降级为 mTLS+口令半免密。

## Criteria

### FK 实测闸门(先行,结论决定腿形)

- [x] lan-linux2 实测 falkordb:4.20.6 镜像:BUILD_TLS 在,`--tls-port` 族可起(S004)
- [x] 实测 `tls-auth-clients-user` 语义:取值 CN/off;CN 模式下客户端证书 CN 映射 ACL 用户,零口令 PONG、GRAPH.QUERY 通(S004)
- [x] 实测结论立档(docs/research/S004-fk-mtls-probe.md);**腿形裁定:全免密 mTLS**(8.6 旗标名 tls-ca-cert-file 等坑在档)

### FK 实施(腿形依实测定)

- [x] falkordb start 证书面:服务器证书 tar 注入(沿 REQ-0016 机制,uid 0 按 falkordb 镜像实测)+ TLS 旗标/配置 + 客户端证书道(fred v10,redis-rs 无注入口,S004 补记)
- [x] `--auth password` 回落语义与 pg 对齐;ServerInfo.tls 复用
- [x] dotenv/tls/信封文案三面与 pg 腿同构
- [x] lan-linux2 实弹:电池 scripts/test-falkordb-integration.sh 十用例 10/10(服务器面容器内直轰);fred 腿 opt-in live 测试 1/1(宿主 127.0.0.1 经 python 转发器抵容器桥 IP,SAN 校验真实发生;该 daemon 发布口宿主 loopback 不可达,直连发布口待可达环境复验)

### CH 实测闸门(先行,结论决定腿形)

- [x] lan-linux2 实测 clickhouse 26.8:config.d 注入 openSSL/server strict + https_port/tcp_port_secure 起活(S005)
- [x] 实测零口令查询:https + 客户端证书 CN 映射 + `X-ClickHouse-SSL-Certificate-Auth: on` 头(S005)
- [x] 实测结论立档(docs/research/S005-ch-mtls-probe.md + 源码级检索件 s005-ch-config-research-grok.md);**腿形裁定:https_port 承接发布口 + users.d 证书用户 + HTTP 认证头**

### CH 实施(腿形依实测定)

- [x] server start 证书面:证书 + config.d + users.d tar 注入(uid 101),https_port 承接发布 HTTP 口,tcp_port_secure 9440 发布到 native 口,明文口挪冷端口(S005)
- [x] http_query 带客户端身份(reqwest tls_certs_only + Identity + 认证头;交互 REPL 走容器内 clickhouse-client --secure --config)
- [x] dotenv 证书形;`--auth password` 回落;named user 在证书面报用法错(镜像仅 default 用户)
- [x] 实弹:CH 电池 10/10(lan-linux2;password 面用例在该机记注 SKIP = 宿主不可达发布口,CI runner 真跑);PG 电池复跑 14/16(F1 修复实证:cert 面 start/dotenv/resume 全绿,两败皆宿主连接类环境限制);宿主 https 腿的错误文案实证(trust chain 与可达性自撰句)
- 记档:证书面 readiness 刻意不探宿主发布口(发布口解耦设计,S005/电池头在档);宿主客户端是发布口不通的第一发现人,CI runner 与可达环境由电池/实弹覆盖(grok-G1 不采纳的理由)
- 记档:FK 证书面 Browser 证断(2026-10-08 实测:TLS-only 监听关明文后 Browser 后端连不上、3000 口不起);处置 = 证书面 dotenv 不出 FALKORDB_BROWSER_URL、start 输出 browser 行隐藏并注记

### 共通

- [x] README/CONTEXT 帮助面三引擎口径一致(三引擎证书面口径齐;契约账并收:三引擎证书面 start 输出不再打印无效 Password 行)
- [x] 测试:假 Docker 断言 + 真机实弹 + 电池用例(三引擎三面全齐;CH 370/0 + 电池 10/10)
