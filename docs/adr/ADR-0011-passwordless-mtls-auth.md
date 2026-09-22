---
id: ADR-0011
title: 免密密钥身份认证:本地 CA 与三引擎 mTLS 架构
status: accepted
date: 2026-09-22
deciders: [ray]
supersedes: []
superseded_by: null
tags: [auth, tls, certificates]
---

# 免密密钥身份认证:本地 CA 与三引擎 mTLS

## Context

需求: REQ-009;研究底稿: S003。三引擎 client 均支持客户端证书 mTLS(PG 经 tokio-postgres-rustls、FK 经 redis-rs rustls、CH 经 clickhouse-rs with_http_client 注入 reqwest Identity)。用户令分批实施,PG 先行。

## Decision

1. **本地 CA**:dctl 自管一个全局单 CA(`~/.dctl/ca/`,rcgen 纯 Rust,密钥 0600,零入仓零 argv 零日志,同 ledger/registry 密档纪律)。首次使用幂等生成,不复核指纹(开发工具 CA,非生产 PKI)。
2. **证书拓扑**:每实例 start 签发两枚证书,即服务器证书(CN=容器名,供客户端验证)与 dctl 客户端证书(CN=DB 用户名,cert 认证法的身份映射)。私钥全部 0600 落 CA 目录。
3. **PG 接入**:容器挂载证书三元组(server cert/key + CA),PostgreSQL `ssl=on` + pg_hba `hostssl all all all cert`(证书 CN = 用户名即免密)。client 经 tokio-postgres-rustls 带客户端证书连接。
4. **口令回落**:证书道为默认,`--auth password` 旋钮回落到口令连接(迁移与排障不断路);dotenv 按所选道出变量。
5. **FK/CH 后续**:FK 走 redis-rs TLS(基座 8.6+ `tls-auth-clients-user CN` 语义待实测,不支持则 mTLS 加口令半免密);CH 走自建 reqwest HTTP 客户端的 `identity()` 加根证书(仓内已有面,同 S003 考证)。

## Consequences

- 证书目录多一个需保护的 CA 私钥(`~/.dctl/ca/ca.key`),与 ledger pem 同等级。
- TLS-only 意味着明文口令不再出现在 dotenv 输出(密码字段变证书路径)。
- 每实例 start 多一步证书签发(毫秒级 rcgen,可忽略)。
- FK 内嵌基座版本是最大不确定性,留实测闸门。
