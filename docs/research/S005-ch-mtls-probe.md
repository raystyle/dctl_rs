# S005:ClickHouse mTLS 实测闸门(REQ-0017 CH 腿先行判据)

- 日期:2026-10-08;环境:lan-linux2(Docker 29.8.1),镜像 clickhouse/clickhouse-server:26.8(26.8.9.10)
- 结论:**全免密 mTLS 路线成立**,腿形 = https_port 承接现发布 HTTP 口 + strict 客户端证书 + users.d 证书用户(common_name CN 映射)+ HTTP 认证头
- 研究件:docs 下 s005-ch-config-research-grok.md(源码级检索,tag v26.8.9.10-lts)

## 实测事实

| 探针 | 结果 |
| --- | --- |
| config.d 注入 | openSSL/server(certificateFile/privateKeyFile/caConfig/verificationMode strict/loadDefaultCAFile false)+ https_port 8123 + tcp_port_secure 9440 + 明文两口挪冷端口(9400/9500):merge 正常,起 |
| 客户端证书强制 | strict 下无证书握手即拒(非 401);证书不受信 CA 签发同样拒 |
| 零口令查询 | https + 客户端证书(CN=default)+ 头 `X-ClickHouse-SSL-Certificate-Auth: on` + `X-ClickHouse-User: default`:ping=Ok.,`SELECT 42`=42 |
| 认证头开关 | 握手出示证书但不带该头:走口令路径,纯证书用户拒(403/516);带头时 URL 带 user/password 参数反被拒(混用法禁止) |
| native secure | `clickhouse-client --secure --port 9440 --config cli.xml`(openSSL/client 段 certificateFile/privateKeyFile/caConfig)= SELECT 43;无 --cafile 旗标(不存在),证书全走 config |
| entrypoint | users.d 先于首启动注入 zz- 文件后,manage_clickhouse_user 的哈希比较不再写 default-user.xml(锁网臂不触发) |

## 关键坑(实施必守)

1. **认证法互斥**:镜像主 users.xml 的 default 自带空 `<password>`(也是口令法);users.d 必须以 `<default replace="replace">` 整节替换并**重写全部要保留的项**(networks/profile/quota/access_management),只留 ssl_certificates 一法。
2. **pattern 标签名是 `common_name`**(可重复;SAN 形是 `subject_alt_name` 带 `DNS:` 前缀);`<cn>` 报 Unknown certificate pattern type。
3. **users.d 文件名字典序**:要排在 entrypoint 写的 `default-user.xml` 之后(如 `zz-dctl-user.xml`),否则容器层残留的锁网文件后合并会把 networks 盖回 localhost,发布口进来的桥地址客户端被拒。
4. **注入时序**:材料与 users.d 必须在 create 与首 start 之间进容器(dctl 的 tar 道;docker cp 到 created 容器要求目标父目录存在,整目录拷贝可行)。
5. **HTTP 认证头**:必须 `X-ClickHouse-SSL-Certificate-Auth: on`(值精确 on)+ `X-ClickHouse-User`;禁止 query user/password、`X-ClickHouse-Key`、Authorization。原生协议相反:配置带证书即证书认证,口令被忽略,无回退(23.3+)。
6. **证书属主/模式**:CH 以 clickhouse 用户读;产品道走 tar 注入 uid 101 + key 0600(探针以 644 过语义)。
7. **明文口挪走别删**:`<tcp_port>` 删键会让 entrypoint 的 init 客户端拿空端口;挪冷端口不发布即可。证书面不设 CLICKHOUSE_DB(则 init 跳过),数据库由证书客户端 CREATE。
8. **reqwest 面**:0.13 rustls 要用 `tls_certs_only([Certificate::from_pem(ca)])` 只信任本 CA(勿用 deprecated add_root_certificate,平台校验器会报 UnknownIssuer);`Identity::from_pem(key_pem + cert_pem 拼接)`。
9. **server.key 权限错误形态**:Poco Permission denied(不是文件不存在);tar uid 错了第一现场就在这。

## 对 REQ-0017 的裁定输入

- CH 腿形 = https_port 承接现发布 8123(容器内口不变,HostConfig 不动)+ tcp_port_secure 9440(发布到现 native 口)+ users.d 证书用户(CN=dctl 用户名)
- `--auth password` 回落 = 现状 env 道(CLICKHOUSE_USER/PASSWORD/DB)
- 客户端:HTTP 查询走 reqwest(tls_certs_only + Identity + 两头);交互 REPL 走容器内 clickhouse-client --secure --config(tar 带 cli.xml)
