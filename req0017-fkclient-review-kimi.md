# REQ-0017 FK 客户端 fred mTLS 腿评审(kimi 腿,静态正确性)

- 基线:origin/main = 4e6bba0;待审 HEAD = 031391c(单笔,13 文件 +1706/-61,与申报一致)
- 取证面:全部对推送树(`git show 031391c:...`);对照件 = vendored falkordb-0.10.3(parser/mod.rs、graph_schema/mod.rs、value/vec32.rs)、vendored fred-10.1.0、redis 8.6 redis-cli.c(upstream 源)
- 门禁复跑(本机,031391c 干净工作区):fmt OK;双 clippy 绿(仅存量 proc-macro-error2 future-incompat 警告,非本批引入);cargo test 364 过 0 败 1 ignored(opt-in live,与申报一致);check-md 49 文件干净;分类器测试 OK
- 结论:**F×2 必修,不 CONFIRM**(门禁语句:达成一致前不合 main)

---

## F1(必修,机制级):fk_tls_cli_flags 把「旗标 空格 路径」塞进单个 argv 元素,证书面容器内 redis-cli 全灭

**位置**:crates/databasectl/src/local/docker.rs:1452-1461(fk_tls_cli_flags);两个调用点 docker.rs:986(就绪探针 falkor_is_ready_with_face)与 docker.rs:1439(交互 exec exec_redis_cli_in_container_with_face)。

**机制**:`format!("--cert {FK_TLS_DIR_IN_CONTAINER}/client.crt")` 产出的是**一个**带内嵌空格的 argv 元素。docker exec 的 `cmd` 数组直达 OCI process.args,**无 shell 分词**(exec_command_tty 与探针都把 cmd 直塞 ExecConfig.cmd,docker.rs:986-1000/1499 实证)。redis-cli 的 parseOptions 是 `!strcmp(argv[i],"--cert") && !lastarg` 后取 `argv[++i]`(redis 8.6 redis-cli.c 3014-3019 行,upstream 源核),单元素 "--cert /path" 精确匹配落空,跌入兜底:「Unrecognized option or bad number of args for: '--cert /var/lib/falkordb/tls/client.crt'」→ **exit(1)**(同文件 3057-3062 行)。

**失败场景**(真 Docker 下,假夹具全部照绿):
1. 证书面是默认面(cli.rs `default_value_t = AuthFaceArg::Cert`)。`dctl fk start` → 探针 exec exit 1 → falkor_is_ready_with_face 返回 Ok(false)(docker.rs 尾段 exit_code==Some(0) 判定)→ 重试至 wait_timeout → start 报就绪超时并回滚。**默认面 fresh start 与 resume 在真机上 100% 失败**。
2. 证书面交互腿 `dctl fk client`(无 -q、TTY)→ redis-cli 即死 exit 1,REPL 起不来。
3. 口令面两腿不受影响(走 REDISCLI_AUTH env,不碰该函数)。

**为何现有门禁没抓到**:假 Docker 夹具钉的就是错误形状——local_falkor_readiness_test.rs 新用例断言 `cmd.iter().any(|arg| arg.starts_with("--cacert "))`(带空格的前缀),把 bug 钉成了契约;电池脚本里 `$FK_TLS_CLI` 是不加引号的 shell 展开,分词正常,所以电池自己的容器内直轰是对的,但电池用例 1/2/3/6/7/8/9/10 的 `dctl fk start`(默认证书面)会经 dctl 内部探针全部踩雷。REQ-0017 自记「lan-linux2 实弹…电池已建,实弹待」未勾选——正是这道未跑的闸门该拦住的类。

**修法**:拆成独立元素——
```rust
["--tls", "--cert", &client_crt, "--key", &client_key, "--cacert", &ca_crt]
```
(每旗标两元素);夹具断言同步改为 `cmd.contains(&"--cacert")` 加相邻位配对断言。修复后合 main 前在 lan-linux2 跑电池(REQ 自己的未勾判据),F1 的实证闭环即由用例 1/6 提供。

## F2(必修,仓规 Must 级):命令面文档漂移——README 的 FK 节与 AGENTS.md 仍写口令面默认

**位置与失败场景**:
1. README.md:150 `$ dctl falkordb start   # 默认随机密码`——错,默认已是证书 mTLS(`--auth` default Cert)。用户照文档预期一个可用密码,实际得到零口令证书面,dotenv 里也没有 FALKORDB_PASSWORD。
2. README.md:153 `dotenv # 写 FALKORDB_HOST/PORT/PASSWORD/BROWSER_URL`——证书面写的是 FALKORDB_TLS/CA_CERT/CLIENT_CERT/CLIENT_KEY 四键无口令(falkordb.rs dotenv 双臂实证)。
3. README FK 节(146-155)整节未提 `--auth password` 回落;PG 节在 REQ-0016 已得全量待遇(136-144 行,默认面/回落/dotenv 两形),FK 节未对齐。本批只改了 README 行 3。
4. AGENTS.md:3 「FalkorDB 内置官方 falkordb」——证书面已走 fred;仓规「改动 AGENTS.md 记录的内容须同步更新」适用(README 行 3 改了,AGENTS.md 同句没改)。

仓规原文:「改命令面同步更新 clap 定义内帮助文本与 README」。REQ-0017 共通项「README/CONTEXT 帮助面三引擎口径一致(FK 面已改…)」虽标 [ ],但括注声称 FK 面已改,与 146-155 节实态不符。

**修法**:FK 节按 PG 节同构改写(默认证书面、`--auth password` 回落、dotenv 两形、resume 保面);AGENTS.md 行 3 补「证书面 fred」。cli.rs:406 `--password` 帮助文案未注明口令面限定,可顺手(PG 同位置同样沉默,属两腿共有的小瑕疵,不单列)。

---

## G(建议,采纳与否留白)

**G1 start() 注释自相矛盾**:falkordb.rs 约 458 行「The client pair is uploaded too: the in-container redis-cli legs (readiness, interactive, **and the certificate-face programmatic path** …) ride it」——证书面程序化道恰恰不再走容器内 redis-cli(本提交的核心主张,ADR-0009 追注明写该过渡道不采用)。骑手名单删掉程序化道即可。

**G2 信封纪律**:fred 腿把库文本并进 FalkorUsage parity 信封(falkordb.rs:1446 init 失败带 `{error}`、1572 GRAPH.QUERY 带 fred 文本、1643 schema refresh 同),而 REQ-0016 二轮已裁定 PG Required 双臂只装自撰句(我当时的 F2)。缓辩:FK 口令面既有码本就把 falkordb crate 错误原文进信封(2156/2162,存量),且查询错误的服务器正文(Cypher 语法错)本就是产品面,必须到达用户。建议:查询错误放行,**连接错误**改自撰句对齐 PG 腿;或把「查询腿引擎正文放行」明记为教义例外。两腿各执一词时留下一批统一裁定亦可。

**G3 fk start --auth 无 try_parse_from 解析测试**:pg 侧有 postgres_start_auth_face_parses_with_cert_default,fk 侧新旗标只有假 Docker 端到端覆盖(--auth password 与默认两路都经真二进制,覆盖其实在),缺仓规要求的解析单测。照 pg 样补一条即可,便宜。

**G4 FK tar 的 mode/uid 无断言**:PG tar 有单测钉 uid/gid/0600;FK tar 只有夹具的路径名子串断言,0600→0644 的回归会漏网。照 PG 样给 build_falkordb_tls_tar 补一条单测。

**G5 证书面 Browser 未探测**:`--port 0` 关掉容器内明文监听,FalkorDB Browser(3000 口)大概率经容器 loopback 明文连 redis,证书面下疑似断;S004 未探,dotenv 证书面仍出 FALKORDB_BROWSER_URL。建议在 lan-linux2 电池补一探针;若实断,证书面 dotenv/start 输出对 browser URL 加以注记或抑制。

**G6 证书面 start 输出打印伪造 Password 行**:output.rs:767,FalkorStartOutput.password 是生成的随机串但证书面从未装上服务器(REDIS_ARGS 无 requirepass),人类面与 JSON 面都带一个不能认证任何连接的「密码」。与 REQ-0016 已记档的 PG 证书面 Password 行同族,归契约统一裁定账时把 FK 一并纳入即可,不单设处置。

---

## 七问裁决(申报重点逐项)

1. **解码器镜像保真:过**。逐行对照 vendored falkordb-0.10.3:标记表 1-16 全同(parser/mod.rs:14-31);header 二元组取第二槽、余取首槽同(:216-243);Node [id,labels,props] / Edge [id,rel,src,dst,props] / Path [nodes,edges] / props [key,marker,val] 三元组同;schema 懒刷新 + 刷新后仍缺即错同(MissingSchemaId 语义,graph_schema/mod.rs:198-219);刷新按列举序 enumerate 建 id 同;Bool/F64/Vec32 字符串解析同;Vec32 未导出改 Debug 镜像串与 `#[derive(Debug)] Vec32 { values }` 逐字同(value/vec32.rs:12-16)。schema 刷新用 GRAPH.RO_QUERY 而官方用 GRAPH.QUERY——行为等价且更严(只读道),非缺陷。CLI 读取回复无 stats 时静默丢行的边角与官方假设一致,服务器恒发 stats。
2. **fred 生命周期:过**。fred-10.1.0 默认 RESP2(config.rs:690,`version: RespVersion::RESP2`),解码器无 RESP3 map 编码险;TlsConfig::from(rustls ClientConfig) 在(protocol/tls.rs:215,enable-rustls-ring 已开);init/quit 配对,quit 结果忽略合理;connect/query 错误经 FalkorUsage 出(口径见 G2)。移动语义编译面已证。
3. **client() 分派:过(设计面)**。direct 走 falkordb crate 无 TLS(帮助文案口径一致,可接受);interactive 留容器内 exec 合 ADR-0009(但被 F1 实际打断);证书面程序化走 fred 宿主直连;口令面不变。`read_fk_password` 在证书面得空串且不进入任何 TLS 路径,password_from_redis_args 对 TLS REDIS_ARGS 返回 "",无副作用。
4. **删包装与改名:过**。falkor_is_ready/exec_redis_cli_in_container 旧名全仓零残留(git grep 031391c 实证,余者为同名 trait 方法);postgres.rs 为纯改名,六处 PostgresAuthArg→AuthFaceArg 无行为漂移。
5. **夹具与电池:部分过**。夹具 PUT archive 接受、tar 五文件名、REDIS_ARGS TLS 形状、无 auth env 断言俱在——但探针 argv 断言钉的是 F1 的错形;电池十用例断言面设计合理(含 orphan 恢复的 tls 面、双实例端口、resume 保面),然未跑(REQ 自记),F1 证明它必须跑。
6. **Box::pin:过**。decode_falkor_value↔decode_typed 环在 Array/Map 两处断、decode_typed→node/edge→properties→decode_typed 环在 properties 处断,编译器可 sizing;必要性成立(async 递归),深度随回复嵌套由服务器侧 bounding,本地可信面可接受。
7. **文档口径:不过(见 F2)**。S004 补记、ADR-0009/0011 追注、REQ 勾选(含实弹未勾的诚实记账)、README 行 3 与码实态一致;README FK 节与 AGENTS.md 行 3 漂移。

## 其他已核无缺陷点(备查)

- tar 注入回滚完备:签发+上传并入 startup_result、置于 start_existing 前(沿 REQ-0016 二轮修法),上传失败/启动失败/就绪失败三路径都入 rollback_failed_fresh_start,材料随容器层清除,无半态。
- ServerInfo.tls 兼容面:旧元数据 None=口令面;recover 从 REDIS_ARGS 含 `--tls-port` 推导,inspect 失败 continue 不误分类;resume 用 prior.tls==Some(true);口令面 REDIS_ARGS 无该子串,密码校验禁空白使伪造注入不可能。
- dotenv 剥离:精确键匹配 + export 前缀处理,update_dotenv 对「前缀命中但不在 vars」的键保留原行(mod.rs:266),故四面 TLS 键的双手动剥离是必要的且双向切换干净;链式 once("") 保住尾换行;format_dotenv_line 对含空格值加引号既有测试在。
- 85cf0b9 的 uid 教训已吸收:FK tar uid/gid 0 与 run.sh root 起 redis 的实态一致,S004 坑 3 在档。
- 证书面 --password 静默忽略与 PG 腿同构(两腿都不拒绝),不列缺陷。
