# REQ-0017 FK 客户端 fred mTLS 腿 评审回执 (grok)

批次: `031391c` on `feat/req0017-fk-mtls-fred-client`,基线 `origin/main` `4e6bba0`. 单提交,13 文件,+1706/-61. 只评审,不改产品码.

**不放行. 无 CONFIRM.** 四处必修,四处建议.

本批实际同时落地了 FK 服务端证书面(tar、REDIS_ARGS、探针、孤儿恢复). 回执覆盖整提交,不限于解码器.

## 门禁 (本轮复跑)

`cargo fmt --all -- --check` 退出码 0 (检查模式,未改写树). `cargo clippy -p databasectl --all-targets -- -D warnings` 退出码 0. `cargo clippy -p databasectl -- -D warnings` 退出码 0. `cargo test -p databasectl` 退出码 0: 364 passed, 0 failed, 1 ignored (即 `tls_cypher_live_round_trip`). `python3 scripts/check-md.py` 退出码 0 (49 个文件). `python3 scripts/tests/test_classify_install_integration.py` 退出码 0 (7 tests).

## F1 探针与交互式 redis-cli 的 TLS 旗标粘在同一个 argv 里

`fk_tls_cli_flags` (`crates/databasectl/src/local/docker.rs:1452-1461`) 把路径写进旗标字符串本身: `"--cert /var/lib/falkordb/tls/client.crt"` 是一个元素. 调用点是就绪探针 (`docker.rs:985-986`) 和交互式 exec (`docker.rs:1438-1439`). Docker exec 的 Cmd 不经 shell,不会再拆空格.

redis-cli 8.6 (`src/redis-cli.c:3012-3017`) 用 `strcmp` 对整个 argv 元素比对 `"--cert"` / `"--key"` / `"--cacert"`,路径取下一个元素. 带空格的那一个元素对不上,客户端证书根本没装上,进程以 unrecognized option 退出. 证书面 fresh start 的探针永远拿不到 PONG,等满 `wait_timeout` 后回滚,实例起不来. 交互式 `falkordb client` 同一组旗标,REPL 同样连不上.

假夹具把这个形状钉死了: `crates/databasectl/tests/local_falkor_readiness_test.rs:616-619` 断言 `arg.starts_with("--cacert ")`. `cargo test` 因此仍绿. 电池脚本自己的 `FK_TLS_CLI` 是 shell 分词,那条直轰是对的;但它先走 `dctl start`,会先死在这根探针上. 实弹框未勾 (`REQ-0017` 第 28 行) 与此相符,不能当成已经覆盖.

修法: 旗标与路径分成六个 argv 元素 (`--tls`, `--cert`, 路径, `--key`, 路径, `--cacert`, 路径). 夹具改为断言独立元素,不要 `starts_with("--cacert ")`.

## F2 Vec32 单元格与口令面不同构

口令面渲染走 `render_falkor_value` 的 `other => format!("{other:?}")` (`falkordb.rs:2204`). `FalkorValue::Vec32` 的派生 Debug 是 `Vec32(Vec32 { values: [...] })` (vendored `falkordb-0.10.3` `src/value/mod.rs:51-52` 加 `src/value/vec32.rs:12-16`).

证书面因为 `Vec32` 未导出,改成 `FalkorValue::String` (`falkordb.rs:1967-1972`),字符串是 `Vec32 { values: {values:?} }`. `String` 臂 (`falkordb.rs:2180`) 原样吐出,没有外层枚举名. 同文件注释写 "mirror that text exactly",镜像的是内层结构体,不是口令面实际印出的枚举 Debug.

失败场景: `RETURN vecf32(...)` 或节点上的向量属性. 同一实例两条面,证书面印 `Vec32 { values: [1.0] }`,口令面印 `Vec32(Vec32 { values: [1.0] })`. 本批的同构契约在这一型上不成立. 现有六个解码单测没有覆盖 marker 12,所以没人抓住.

修法: 字符串写成完整枚举 Debug `Vec32(Vec32 { values: ... })` (类型在 crate 外仍不可命名,继续用 String 承载). 补一条 marker 12 单测,断言与 `format!("{:?}", FalkorValue::Vec32(...))` 同文. 属性里的向量走同一条 `render_falkor_value`,一并盖住.

## F3 新 fred 错误把库的 Display 并进了 parity 信封

`Error::FalkorUsage` 是 parity (`crates/databasectl/src/local/output.rs:332`),`message` 等于 Display. 自撰句可以进信封;驱动器/库正文不行. 这是 PG 腿已关闭的纪律.

新腿三处把 fred 的 `Display` 拼进了 `FalkorUsage`:

- 连接失败, `falkordb.rs:1445-1448` (`with the client certificate: {error}`)
- `GRAPH.QUERY` 失败, `falkordb.rs:1571-1572` (`failed: {message}`, `message` 来自 `error.to_string()`, `falkordb.rs:1500`)
- schema 刷新的传输失败, `falkordb.rs:1640-1643`

失败场景: 证书不受信、握手告警、或服务端 `ERR`. `--json` 的 `message` 变成 fred/redis 正文,而不再是本仓自撰句.

同提交的上传路径是对的: `docker.rs:660-668` 把 daemon 文案打到 stderr,信封只留自撰句. 新查询腿应照这个拆,不要照下面那条旧腿.

不另立: 口令面 `run_native_cypher` (`falkordb.rs:2138-2162`) 在 main 上已经同样把 `{error}` 拼进 `FalkorUsage`. 那是既有债,不是本批引入的. 修 F3 时不要顺手改口令面,除非另开一轮.

`docs/requirements/REQ-0017-fk-ch-mtls.md:27` 已勾选「dotenv/tls/信封文案三面与 pg 腿同构」. dotenv 与 tls 位是齐的;信封文案不齐. 这行在 F3 修掉之前不能算兑现.

## F4 FalkorDB 命令的 CONTEXT 仍按口令世界写,和证书面默认相矛盾

`crates/databasectl/src/local/cli.rs:327-330`:

- 「resumed with its stored password」. 证书面实例没有 requirepass;resume 沿用的是存储的面 (`falkordb.rs:422`, `387-400`).
- 「The generated password is printed once by start, re-read it later with `falkordb dotenv`」. 证书面 dotenv 不写 `FALKORDB_PASSWORD` (`falkordb.rs:2265-2276`). 默认 start 印出的口令没有装进 `REDIS_ARGS` (`docker.rs:814-820`).
- resume 忽略列表写了 `--port/--browser-port/--password/-e`,没写 `--auth`. 代码在 `falkordb.rs:387-392` 把显式 `--auth password` 一并忽略 (只警告,不改面).

`falkordb start` 自己的 CONTEXT (`cli.rs:368-374`) 完全不提证书面默认. PG 的对应块 (`cli.rs:790-792`) 写明 fresh start 是证书面,印出的口令只在 `--auth password` 上有意义. `--password` 的帮助 (`cli.rs:406`) 仍说这就是 Redis 口令,没有「只在 password 面生效」.

失败场景: 代理读 CONTEXT,跑默认 `falkordb start`,抄走印出的口令,再 `falkordb dotenv` 发现没有 `FALKORDB_PASSWORD`;或在 resume 上加 `--auth password` 以为能换面,存储的证书面不动.

`--auth` 旗标自己的一行说明 (`cli.rs:430-431`) 是对的,盖不住 CONTEXT 里那两句假话. REQ 第 38 行把「三引擎帮助面一致」留空并注明 CH 未做,这不授权 FK 块里已经写错的句子.

## G1 schema 刷新命令与官方 crate 不一致,且没有任何测试真的发出去

采纳/不采纳:

官方 `graph_schema/mod.rs:154-162` 用 `GRAPH.QUERY` 加 `CALL DB.LABELS()` / `DB.PROPERTYKEYS()` / `DB.RELATIONSHIPTYPES()` (`同文件 15-20`). 本仓用 `GRAPH.RO_QUERY` (`falkordb.rs:1512-1517`) 加 `db.labels` / `db.relationshipTypes` / `db.propertyKeys` (`falkordb.rs:1587-1593`).

回包解析对齐的是非 compact 的 `[header, rows, stats]`、首槽字符串 (`falkordb.rs:1525-1557`),这一层是对的. `GRAPH.RO_QUERY` 是只读孪生,过程名在 FalkorDB 里大小写不敏感,所以不升 F. 但 `CannedProcedureCaller` 不检查命令名,电池是容器内 redis-cli,不走 fred. 唯一会打到这条命令的是 `#[ignore]` 的 `tls_cypher_live_round_trip` (`falkordb.rs:2641`, MATCH 返回节点才会刷新).

若服务端拒绝 `GRAPH.RO_QUERY` 或过程名,标量查询仍成功,节点/边单元格在刷新时报 `schema refresh failed`. 建议要么改成与 vendored `refresh` 相同的 `GRAPH.QUERY` + 大写过程名,要么在实弹里把节点单元格断言留成必跑.

## G2 start 注释仍说证书面程序化查询走容器内 redis-cli

采纳/不采纳:

`falkordb.rs:453-458` 写客户端证书对要给「readiness, interactive, and the certificate-face programmatic path」用. 程序化道已经是宿主 fred (`falkordb.rs:1400-1405`). 容器内这对证书现在只服务探针和交互式 exec. 注释会把下一次修改引回已放弃的 exec 过渡道.

## G3 ADR-0009 追注丢了「与」

采纳/不采纳:

`docs/adr/ADR-0009-native-client-integration.md:34` 写「渲染面两口令面同构」. S004 (`docs/research/S004-fk-mtls-probe.md:33`) 是「渲染面与口令面同构」. 缺一字之后读成「两个口令面」. 同构本身还被 F2 挡住,这句话在 F2 修完之前也不成立.

## G4 假夹具的 tar 只钉了五个路径名

采纳/不采纳:

`local_falkor_readiness_test.rs:593-601` 用 `tar.contains(entry)` 钉五文件名. `build_falkordb_tls_tar` (`docker.rs:605-617`) 的 uid 0、key `0o600`、证书 `0o644` 没有断言. 模式或属主回退时,这条测试仍绿. REDIS_ARGS 的 TLS 形状 (`568-580`) 和探针无 auth env (`621-625`) 是钉住的;argv 粘连见 F1.

## 已核对,不立项

解码器对照 vendored `falkordb-0.10.3` `src/parser/mod.rs` 与 `src/graph_schema/mod.rs` (除 F2、G1 外):

- 标记 1 到 16 与 `ParserTypeMarker` 一致 (`falkordb.rs:1733-1749`,官方 `parser/mod.rs:14-31`). 未知标记拒绝.
- header: 长度恰为 2 取第二槽,否则取第一元素 (`falkordb.rs:1832-1836`,官方 `parser/mod.rs:223-236`).
- Node 三元组、Edge 五元组、Path 二元组里的节点/边是裸元组而不是再包一层类型标记 (`falkordb.rs:2002-2057`, `1903-1924`;官方 `graph_entities.rs` 与 `path.rs`).
- 属性 `[key id, marker, value]` (`falkordb.rs:2071-2088`;官方 `FKeyTypeVal`, `graph_schema/mod.rs:39-56`).
- schema id 未命中时整表刷新一次,仍没有则报错 (`falkordb.rs:1616-1630`;官方 `parse_single_id` 的 `MissingSchemaId`). 枚举序即 id.
- Bool 只认 `"true"` / `"false"` 字符串;F64 与 Vec32 分量都是字符串再解析 (`falkordb.rs:1870-1881`, `1958-1966`;官方 `redis_value_as_bool` / `redis_value_as_double` / `redis_value_as_float`).
- 时间标量走公开的 `DateTime::new` 等,再进共享渲染器的 Debug 臂,两条面同构.
- 回包先丢掉 stats 尾;剩下长度为 2 才是 header+rows,否则空行 (`falkordb.rs:1788-1802`).

`Box::pin` 两处都是真递归,不是多余的:

- 数组元素与 map 值: `decode_typed` 到 `decode_falkor_value` 再回到 `decode_typed` (`falkordb.rs:1891`, `1938`).
- 属性值: `decode_typed` 到 node/edge 到 `decode_properties` 再回到 `decode_typed` (`falkordb.rs:2087`). Path 直接调 `decode_node` / `decode_edge`,不需要第三处. 深度跟着回包,服务端有界.

fred 生命周期: `TlsConfig::from(ClientConfig)` 吃掉配置,之后没有再用 (`falkordb.rs:1438-1442`). `init` 失败直接返回,连接还没起来,不 `quit` (`1445-1449`). 查询无论成功失败都 `quit` (`1450-1459`). 这一层没有问题;问题在 F3 的文案.

`client()` 四路 (`falkordb.rs:1339-1414`): `--host`/`--port` 仍是无 TLS 的官方 crate (帮助 `cli.rs:497` 写明任意 Redis 协议服务,与 ADR-0009 直连口径一致). 交互式 (无 query 且 stdin/stdout 都是终端) 两条面都走容器内 exec,符合 ADR-0009 决策 4,不重建 REPL. 证书面程序化走 fred 到 `127.0.0.1:tcp_port`. `tls` 为 `None` 或 `Some(false)` 留在口令面. 证书面 `read_fk_password` 读不到 requirepass 时得到空串 (`falkordb.rs:1204-1220`),随后不使用.

旧包装 `falkor_is_ready` / `exec_redis_cli_in_container` 在 `docker.rs` 已不存在. 全仓只剩 trait 方法 `falkor_is_ready` (`falkordb.rs:882`),那是探针抽象,不是删掉的那个函数.

`postgres.rs` 只把 `PostgresAuthArg` 换成 `AuthFaceArg`,比较仍是 `== Password` / `== Cert` (`postgres.rs:363`, `396`). 全仓已无 `PostgresAuthArg`. 无行为漂移.

签发与上传在回滚块内 (`falkordb.rs:459-491`). 上传失败的人话/信封拆分与 PG 的 `dd8ec52` 同句 (`docker.rs:660-668`). 服务器证书 SAN 含 `127.0.0.1` 与 `::1` (`ca.rs:97-106`),fred 连发布口时 rustls 核的是这个 SAN.

孤儿恢复: `REDIS_ARGS` 含 `--tls-port` 才记 `tls: Some(true)`,inspect 失败则跳过该容器 (`docker.rs:1939-1951`).

电池 (`scripts/test-falkordb-integration.sh`) 覆盖的是服务端面: TLS PING、明文拒绝、dotenv 无口令、口令面往返、两种 resume、孤儿恢复的 `tls == true`、双实例、版本隔离、运行中拒绝删除. 脚本头写明发布口在该环境从宿主 loopback 不可达,所以 fred 不在这十例里. 这个缺口是写明了的,不单列;它盖不住 F1,因为 start 自己的探针会先失败.

文档其余: README 第 3 行的两条客户端加容器内 REPL,与分派一致. S004 客户端裁定与 ADR-0011 第三条追注 (TLS-only、`tls-auth-clients-user CN`、tar uid 0、fred、`--auth password`、CH 未做) 与码的设计一致. ADR-0011 三条追注顺序是健康评审、PG 晚间、FK;前一条末句「FK/CH 腿仍未做」被下一条接上,文件末尾是当前态. REQ-0017 仍是 draft,实弹与 CH 框未勾,这是诚实的. REQ-009 仍写 FK/CH 未做,在本 REQ 还是 draft 时不过时.

不另立: `falkordb.rs:465` 的 CA 读失败用 `Error::Postgres`,与 `ca.rs:16-18` 的 `ca_err` 同一变体 (`issue` / `issue_server_cert` 在这条 start 路径上已经如此). 信封走 redacted 的 `local_error`,不重开那条已关闭的裁定.
