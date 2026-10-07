---
id: REQ-009
title: 免密密钥身份认证:PG 先行批(客户端证书 mTLS;FK/CH 腿拆后续 REQ)
status: draft
priority: should
trace: PG 腿全落地(客户端基建 f7ffced + 服务端面 REQ-0016,lan-linux2 实弹 11/11,ADR-0011 两则追注);FK/CH 腿未做,draft 状态由其撑着。
---

# 三引擎免密密钥身份认证

## Scenario

用户令(2026-09-22):三引擎 client 以免密公私钥(客户端证书 mTLS)连接,替代口令;S003 研究(docs/research/S003)已验三引擎客户端库均支持。分批实施,PG 先行(改动最小、语义最清)。

## Criteria

### PG 先行批(本批)

- [x] dctl 本地 CA(rcgen,`~/.dctl/ca/` 全局单份,密钥 0600)幂等生成(健康评审批补目录锁与原子写)
- [x] 实例 start 签发服务器证书与 dctl 客户端证书(CN=DB 用户名),挂载进容器(REQ-0016 落地,tar 注入容器层)
- [x] PostgreSQL 配置 TLS(ssl=on + 证书三元组)+ pg_hba `hostssl ... cert` 法(REQ-0016 落地)
- [x] client 走 tokio-postgres-rustls(客户端证书连接,免口令)(证书面免口令实证;宿主回环面受测试机 daemon 限制)
- [x] 口令道保留为显式回落(`--auth password` 旋钮,迁移不断路)(REQ-0016 落地)
- [x] dotenv 出证书路径(免密形态)(PGSSLMODE/PGSSLROOTCERT/PGSSLCERT/PGSSLKEY)
- [x] 测试:CA 幂等性、证书形状、TLS 连接真机实弹(lan-linux2 实弹 11/11)

### FK/CH 后续批(另立)

- [ ] FK mTLS(redis-rs TLS + ACL;基座 8.6+ 语义待实测,不支持则 mTLS+口令半免密)
- [ ] CH 客户端证书(clickhouse-rs `with_http_client` 注入 reqwest Identity + CA)
