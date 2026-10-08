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

> 追注(2026-10-08,健康评审):落地进度修正。现行码只含客户端基建(`~/.dctl/ca/` 本地 CA、客户端证书签发、PG client 的 Prefer 先试再带内降级);决策 3 的服务端面(容器 ssl=on、pg_hba cert 法)与决策 4 的 `--auth` 旋钮、dotenv 证书道均未接入,用户可见行为仍为口令道。REQ-009 状态已同步修正为 draft;服务端腿另立 REQ 时从其判据清单迁出。

> 追注(2026-10-08,晚):PG 服务端腿落地(REQ-0016):证书面为 fresh start 默认,服务器证书 tar 注入容器层(uid 999/0600),`-c ssl=on/hba_file` 启动,hba 三行制(local trust、hostssl cert、明文 reject);`--auth password` 显式回落;lan-linux2 实弹 11/11。FK/CH 腿仍未做。

> 追注(2026-10-08,FK 腿):FK 服务端+客户端证书面落地(REQ-0017):TLS-only 监听(--port 0 --tls-port 6379,tls-auth-clients-user CN,S004 旗标名),证书 tar 注入容器层 uid 0;客户端从宿主经 fred 直连 mTLS(见 ADR-0009 追注),`--auth password` 回落保留。CH 腿仍未做。
