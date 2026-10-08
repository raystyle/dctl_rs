---
id: REQ-0017
title: FalkorDB 与 ClickHouse 的 mTLS 腿(ADR-0011 收官;FK 先过实测闸门)
status: draft
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

- [ ] falkordb start 证书面:服务器证书 tar 注入(沿 REQ-0016 机制,uid 按 falkordb 镜像实测)+ TLS 旗标/配置 + 客户端证书道
- [ ] `--auth password` 回落语义与 pg 对齐;ServerInfo.tls 复用
- [ ] dotenv/tls/信封文案三面与 pg 腿同构
- [ ] lan-linux2 实弹(含 redis-cli 经 TLS 的实测通路)

### CH 实施

- [ ] server start 证书面:config.d 注入(openSSL/server 段,证书 tar 进容器 /etc/clickhouse-server/)+ HTTP 口 TLS 化或双口过渡(依 clickhouse 面实测)
- [ ] http_query 带客户端身份(reqwest identity + 根证书,ADR-0011 决策 5 的既有考证)
- [ ] dotenv 证书形;`--auth password` 回落;实弹

### 共通

- [ ] README/CONTEXT 帮助面三引擎口径一致
- [ ] 测试:假 Docker 断言 + 真机实弹 + 电池用例
