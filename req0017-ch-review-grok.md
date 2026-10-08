# REQ-0017 CH 腿评审回执

基线: `origin/main` `05e6826`. 提交: `14fe18c` (`main..feat/req0017-ch-mtls`, 16 文件). 对照: `docs/research/s005-ch-config-research-grok.md`. 未重跑门禁, 未复跑电池. 请求里的 369/0 与双 clippy 按声明收, 不复验.

结论: 不放行. 4F + 2G.

## 与 S005 对齐的部分

1. users.d: `users.d/zz-dctl-user.xml`, `<default replace="replace">`, 子项重写 `networks` `::/0`、`profile`、`quota`、`access_management` 0, 标签是 `common_name`, 无 `password`. 与检索件第 1、4、6 项一致. (`docker.rs` `ch_tls_user_xml`)
2. config.d: `verificationMode` `strict`, `loadDefaultCAFile` false, `cacheSessions` true, `disableProtocols` `sslv2,sslv3`, `preferServerCiphers` true. 明文口挪到 9400/9500 且键还在. `https_port` 8123, `tcp_port_secure` 9440, 发布键是 `8123/tcp` 与 `9440/tcp`. 与第 2 项和实施建议形一致.
3. `https_query`: `tls_certs_only` (不是 `add_root_certificate`), `Identity::from_pem` 单块且 key 在前 cert 在后, 头是 `X-ClickHouse-SSL-Certificate-Auth: on` 与 `X-ClickHouse-User`. URL 只有可选的 `database`, 没有 `user` / `password`. 与第 7 项一致.
4. 证书面 env 三元组都不设, `CLICKHOUSE_DB` 不设. 非 default 库在就绪后由证书客户端 `CREATE DATABASE`. 与第 6 项的正向路径一致. 失败路径见 F4.
5. 交互腿: `--secure --host 127.0.0.1 --port 9440 --user default --config /etc/clickhouse-server/dctl/cli.xml`. `cli.xml` 的 `openSSL/client` 含 `certificateFile`、`privateKeyFile`、`caConfig`、`loadDefaultCAFile` false. 没有不存在的 `--cafile`. 与第 5 项一致.
6. upload-500: 失败走 `Error::ClickhouseUsage`, daemon 文本只在 stderr 的人读行; 夹具断言信封行不含 `upload failed by test`, 且有 DELETE 与元数据删除. 这条契约成立.
7. `main.rs` 注释正名成立: 直接依赖的 rustls 与 fred 走 ring; 锁文件里 ledger-client 的 reqwest 0.12 也带 ring; 本仓 reqwest 0.13 的 `rustls` feature 带 aws-lc-rs. 不再把 aws-lc-rs 记到 tokio-postgres-rustls 上. `ca.rs` 的 `ensure_crypto_provider` 与 `main` 的 `install_default` 仍是 first-call-wins.

## F1 证书面 native 口在恢复和 resume 时仍按 9000 读

证书面创建时发布的是容器口 `9440/tcp` (`docker.rs` 约 1137 行). 两处回读仍写死口令面的 `9000`:

- `list_project_engine` 的 ClickHouse 次端口常量是 9000 (约 1610 行). 恢复把 `secondary_port` 写进 `tcp_port` (约 2177 行). 运行中的证书容器没有 9000 绑定, 恢复出来的 native 口是 0; HTTP 口从 8123 还能读到.
- `resume_existing` 只查 `9000/tcp` (`clickhouse.rs` 约 731 行). 查不到就留下 prior. 恢复后的 0 修不回来.

这是口令面已经修过的「删元数据再 start, 端口必须从绑定刷新」在证书面上的原样复发. 夹具把 inspect 永远渲染成 `9000/tcp` (`local_clickhouse_docker_test.rs` 约 252 行), `tls: true` 时也一样, 所以现有 resume 测试看不出. 后果: 证书实例的 `CLICKHOUSE_PORT` 和 start 输出的 Native 在这条路径上是 0; 容器内 REPL 仍打 9440, 所以交互腿会掩盖它.

## F2 证书面 resume 用明文 HTTP 去探 https 口, 并报成口令被拒

`resume_existing` 在就绪之后无条件调用 `warn_if_credentials_rejected` (`clickhouse.rs` 约 774 行). 该函数走 `http_query`, URL 是 `http://127.0.0.1:{http_port}/` (约 1197 行), 超时 120 秒. 证书面的发布 HTTP 口是 https. 每次证书 resume 都会打一记必然失败的明文请求, 然后打印「rejected the printed credentials」. 证书面没有口令, 这条警告是假的. 新鲜 start 的证书分支没有调用它, 所以只有 resume 中招.

## F3 PG/FK 证书面仍打印 Password 行, JSON 仍带空 password 字段

REQ 与 ADR-0011 追注写的是三引擎证书面 start 不再打印无效 Password 行. `postgres.rs` 与 `falkordb.rs` 把 fresh 证书面的 password 置成空串, 注释也写「prints none」. `output.rs` 的 `PostgresStartOutput` 与 `FalkorStartOutput` 没有 `skip_serializing_if`, Display 仍无条件写 `Password:` (约 742 与 767 行). 人读输出是空的 `Password:` 行, JSON 是 `"password":""`. ClickHouse 两面都省略了 (约 786 与 802 行). 置空字符串没有完成这条契约.

## F4 非 default 库的 CREATE 失败不回滚, resume 也不补建

证书面在元数据已落盘、容器已启动、就绪已通过之后才 `https_query` `CREATE DATABASE` (`clickhouse.rs` 约 463 到 474 行). 失败用 `?` 直接返回, 不走 `rollback_failed_fresh_start`. 容器保持运行. 下一次 start 走 resume, resume 不建库. 用户要的库不会出现, 除非手写 SQL. 默认库 `default` 不走这条, 所以默认路径测不到. 发布口在宿主上不可达时, 非 default 库的 start 会在服务器其实已活的情况下失败, 并留下这个半成品.

## G1 容器内探针只打 /ping, 默认库 start 从不走宿主证书查询

采纳:

不采纳:

`clickhouse_tls_is_ready` 打的是 `https://127.0.0.1:8123/ping` (约 862 行). `/ping` 不认证, 头和 CN 对不对都不影响退出码; `strict` 只保证握手时出示了证书. 默认库不调用 `https_query`, 所以「users.d 配错」和「发布口宿主不可达」都能让 start 报成功. 口令面至少会打发布口上的 `/ping`. 这是有意的发布口解耦, 电池头注释也写了转发器. 建议在容器内探针之后加一次宿主侧 `https_query` 的 `SELECT 1`, 失败与就绪失败一样回滚. 否则宿主客户端是第一个发现发布口不通的人.

## G2 ca 单测的 HOME 置换未恢复

采纳:

不采纳:

`ca.rs` 约 200 行 `unsafe { std::env::set_var("HOME", scratch.path()) }` 符合 edition 2024, 也确实把 CA 写进 scratch. 函数结束不恢复旧 HOME, scratch 目录随后被删. 注释说这是唯一碰 HOME 的单元测试所以不会和兄弟抢. 单元测试是同一个库测试进程并行跑的, 这条保证只在「今天没有别的测试读 HOME」时成立. 建议测完写回原值, 或把断言限制在返回的 PEM 上、用可注入的目录, 不要改进程环境.

## 不另立

- 请求写 18 文件, `git diff --stat` 是 16. 不影响缺陷判断.
- REQ 约 33 行「客户户端」多了一个字. 文档笔误, 不升 F.
- 电池 `scripts/test-clickhouse-integration.sh` 的用例形状与 S005 一致 (容器内 wget 双头, `SELECT 42`, 无证书握手失败, `--secure --config` 的 `SELECT 43`). REQ 自己把实弹标成未做. 本单不把脚本存在当成已跑.
- `cli.xml` 根元素用 `<config>`, 与官方 client 配置同形, 不另立.
