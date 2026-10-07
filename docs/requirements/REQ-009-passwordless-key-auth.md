---
id: REQ-009
title: 免密密钥身份认证:PG 先行批(客户端证书 mTLS;FK/CH 腿拆后续 REQ)
status: draft
priority: should
trace: 客户端基建落地(f7ffced:本地 CA + 客户端证书签发 + PG client Prefer 先试);服务端腿未接——容器 ssl=on/pg_hba cert/`--auth password` 旋钮/dotenv 证书道均不在码,用户可见行为仍为口令道(ADR-0011 追注 2026-10-08,健康评审修正)。服务端腿另立 REQ 时从本 REQ 迁出判据清单。
---

# 三引擎免密密钥身份认证

## Scenario

用户令(2026-09-22):三引擎 client 以免密公私钥(客户端证书 mTLS)连接,替代口令;S003 研究(docs/research/S003)已验三引擎客户端库均支持。分批实施,PG 先行(改动最小、语义最清)。

## Criteria

### PG 先行批(本批)

- [x] dctl 本地 CA(rcgen,`~/.dctl/ca/` 全局单份,密钥 0600)幂等生成(健康评审批补目录锁与原子写)
- [ ] 实例 start 签发服务器证书与 dctl 客户端证书(CN=DB 用户名),挂载进容器(客户端证书签发已有,服务器证书与容器挂载未接)
- [ ] PostgreSQL 配置 TLS(ssl=on + 证书三元组)+ pg_hba `hostssl ... cert` 法
- [ ] client 走 tokio-postgres-rustls(客户端证书连接,免口令)(tls_config 与 Prefer 先试已有,降级腿仍是口令,「免口令」未达)
- [ ] 口令道保留为显式回落(`--auth password` 旋钮,迁移不断路)
- [ ] dotenv 出证书路径(免密形态)
- [ ] 测试:CA 幂等性、证书形状、TLS 连接真机实弹(lan-linux)

### FK/CH 后续批(另立)

- [ ] FK mTLS(redis-rs TLS + ACL;基座 8.6+ 语义待实测,不支持则 mTLS+口令半免密)
- [ ] CH 客户端证书(clickhouse-rs `with_http_client` 注入 reqwest Identity + CA)
