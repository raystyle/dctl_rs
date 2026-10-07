---
id: REQ-0016
title: Postgres 服务端 mTLS 腿(容器 TLS 面落地;判据自 REQ-009 迁出)
status: implemented
priority: should
trace: 服务端腿批落地(判据自 REQ-009 迁出;设计真源 ADR-0011)。实弹 11/11 于 lan-linux2(2026-10-08):证书认证端到端、明文 TCP 拒连、口令面回路、dotenv 两形;宿主 client 腿受该 daemon loopback 发布限制,错误路径与文案实证,留常规环境复验。
---

# Postgres 服务端 mTLS 腿

## Scenario

ADR-0011(accepted,2026-09-22)裁定三引擎免密 mTLS,PG 先行;健康审(2026-10-08)确证服务端腿从未接入。现行码只有客户端基建(本地 CA、客户端证书、Prefer 先试再降级),容器不启 TLS,用户可见行为仍为口令道(REQ-009 已回 draft,ADR-0011 追注在案)。本 REQ 补齐 PG 服务端腿,兑现 REQ-009 的 PG 先行批。

## Criteria

- [x] 实例 start(证书道)签发服务器证书(SAN 含 127.0.0.1/::1,发布口直连面),连同 CA 与 dctl 自管 pg_hba 三元组注入容器;密钥落容器内 postgres 属主 0600(不走宿主 bind-mount,避开跨 uid 权限陷阱)
- [x] PostgreSQL 以 `-c ssl=on -c ssl_cert_file=… -c ssl_key_file=… -c ssl_ca_file=… -c hba_file=…` 启动;pg_hba 为 `local trust`(镜像入口脚本兼容)+ `hostssl … cert`(TCP 免密证书法)+ `host … reject`(拒明文 TCP)
- [x] 元数据记录认证模式(serde 默认缺省 = 旧实例口令道);旧实例 resume 行为不变;fresh start 默认证书道
- [x] `postgres start --auth password` 显式回落口令道(不挂 TLS 面,现状行为);resume 带该旗标属被忽略旗标(与端口/密码同规则)
- [x] 托管 client:证书道实例要求 TLS 连接成功(禁 NoTls 静默回落,回落必得认证失败,报真因);口令道实例维持 Prefer 先试再降级现状
- [x] dotenv 按道出形:证书道出 PGSSLMODE=verify-full + PGSSLROOTCERT/PGSSLCERT/PGSSLKEY 证书路径(libpq 正名),不出 POSTGRES_PASSWORD;口令道不变
- [x] 测试:clap 解析(--auth 两值、默认 cert)、假 Docker create 断言(Cmd 旗标、无 TLS 面时缺省)、resume 旧元数据兼容、dotenv 两形;lan-linux2 真机实弹(免密连接、口令回落、resume)

## 已裁定的设计点(实施时不再议)

- 证书材料注入走容器创建后 tar 上传(bollard upload,tar 条目 uid/gid 999 + mode 0600/0644),非宿主 bind-mount:宿主用户无法 chown 给容器内 postgres uid
- POSTGRES_PASSWORD 仍在创建时注入(镜像入口脚本初始化要求),但证书道 hba 不放行 TCP 口令法,该口令不可远程使用
- 客户端证书沿用 ca::client_cert(CN=DB 用户名,ADR-0011 证书拓扑)
