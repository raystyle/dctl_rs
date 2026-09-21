# S003 三引擎免密密钥身份认证研究

> 本文件 = Postgres/ClickHouse/FalkorDB 三引擎免密密钥认证的机制考证、dctl 集成面评估与设计草案;数据源为各库官方文档与实现仓一手核对(2026-09-22),FalkorDB 内嵌基座的证书级身份语义未经实机验证。结论供 REQ/ADR 裁定,三个裁定点在档。

## 结论速览

| 引擎 | 机制 | dctl 集成面 | 风险 |
| --- | --- | --- | --- |
| Postgres | TLS 客户端证书认证(pg_hba `cert` 法) | tokio-postgres + tokio-postgres-rustls 胶水(直连已原生,加 TLS 分支即可) | 低;证书 CN 必须等于 DB 用户名 |
| ClickHouse | `IDENTIFIED WITH ssl_certificate CN 'user'`,HTTPS HTTP 接口按证书 CN/SAN 认证 | reqwest rustls `Identity` 加根 CA,现有 HTTP 客户端直接升级 | 中;服务端要开 openSSL 且 start 时 bootstrap 用户 |
| FalkorDB | mTLS 传输 + Redis 8.6+ `tls-auth-clients-user CN` 证书映射 ACL 用户 | redis crate TLS 特性(falkordb crate 透传 `rediss://` URL) | 高;内嵌基座版本语义待实测,否则退化 mTLS 加口令 |

## 各引擎事实

### Postgres

- 服务端:`ssl=on` 加证书三元组挂载进容器;`pg_hba.conf` 加 `hostssl all all all cert`(或 scram 加 `clientcert=verify-full` 双因子) [实证: PostgreSQL 官方文档 auth-cert 页 https://www.postgresql.org/docs/current/auth-cert.html]
- 客户端:tokio-postgres 本体不带 TLS,社区胶水 tokio-postgres-rustls 提供 `MakeRustlsConnect`;rustls 以 PKCS8 加证书链构造 `ClientConfig` 传入连接 [实证: crates.io/crates/tokio-postgres-rustls]
- 语义:证书 CN 须等于连接用户名,免密即成 [实证: 同 auth-cert 文档]

### ClickHouse

- 用户面:`CREATE USER dctl IDENTIFIED WITH ssl_certificate CN 'dctl'`;HTTPS 接口下空密码用户按客户端证书 CN/SAN 认证 [实证: ClickHouse 官方指南 https://clickhouse.com/docs/concepts/features/security/ssl-user-auth]
- 服务端:两端口配置文件 `openSSL` 段(服务器证书加客户端 CA `requireClient`);dctl start 需挂 TLS 配置并在启动窗口 bootstrap 该用户 [实证: Altinity 加固系列 https://altinity.com/blog/locking-down-clickhouse-networking-part-2]
- 客户端:仓内 HTTP 客户端为 reqwest 加 rustls,`ClientBuilder::identity` 加 `add_root_certificate` 即接上,改动小 [实证: reqwest 文档与本仓 clickhouse.rs 现行实现]

### FalkorDB

- Redis 8.6+ 有 `tls-auth-clients-user CN` 语义:证书 CN 映射 ACL 用户,零口令 mTLS [实证: go-redis 仓 tls_cert_auth 测试用例 https://github.com/redis/go-redis]
- redis crate TLS 走 rustls 特性,支持客户端证书与自定义 CA;falkordb crate 的 `FalkorConnectionInfo` 接受 `rediss://` URL(内部经 redis::IntoConnectionInfo) [实证: falkordb-rs connection_info/mod.rs 与 redis crate 文档]
- FalkorDB 4.x 内嵌 Redis 基座是否含 8.6 语义待实机验证(`CONFIG GET tls-auth-clients` 支持度) [假设: 待 lan-linux 实测,不支持时走退路]

## dctl 侧设计草案(未立 ADR,待裁定)

1. CA 与凭据面:dctl 自管一个小型本地 CA(全局 `~/.dctl/ca/` 或每项目一份,待裁);每实例 start 时签发服务器证书与 dctl 客户端证书,私钥落 `0600` 密档,同 ledger/registry 纪律(零 argv 零日志零入仓)。
2. 回落兼容:证书道默认,口令道保留为显式回退(`--auth password` 旋钮),迁移与排障不断路;dotenv 输出面按所选道出变量。
3. 分批建议:PG 先行(改动最小、语义最清),CH 次之(服务端配置与用户 bootstrap 是主工作),FK 最后(版本语义待实测)。
4. 端口面:倾向 TLS-only 收口明文口,随 ADR 定。

## 裁定点(给用户)

- CA 范围:全局单 CA(跨项目共享、吊销简单)vs 每项目一 CA(隔离强、管理面多)
- 实施批次:三引擎一次立 REQ 全做 vs 按建议分批(PG 先)
- FK 若基座无 8.6 语义:接受 mTLS 加口令半免密 vs 等 FalkorDB 上游

## 信源

- ClickHouse SSL 用户证书认证指南:https://clickhouse.com/docs/concepts/features/security/ssl-user-auth
- Altinity ClickHouse 网络加固系列二(X.509 用户认证实践):https://altinity.com/blog/locking-down-clickhouse-networking-part-2
- go-redis tls_cert_auth 测试(Redis 8.6+ tls-auth-clients-user 用例):https://github.com/redis/go-redis
- PostgreSQL cert 认证文档:https://www.postgresql.org/docs/current/auth-cert.html
- tokio-postgres-rustls:https://crates.io/crates/tokio-postgres-rustls
