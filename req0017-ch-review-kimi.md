# REQ-0017 CH 腿 + 契约账评审(kimi 腿,静态正确性)

- 基线:origin/main = 05e6826;待审 HEAD = 14fe18c(单笔,16 文件 +1415/-93;申报 18 文件,实为 16,无碍)
- 取证面:对推送树 `git show 14fe18c:...`;对照件 = docs/research/S005-ch-mtls-probe.md、docs/research/s005-ch-config-research-grok.md、docker-library/postgres 18/trixie entrypoint(upstream 源)
- 门禁复跑(本机,14fe18c 干净工作区):fmt OK;双 clippy 净;cargo test **369/0/1**(与申报一致);check-md 53 文件净;分类器 OK
- 结论:**F×5 必修,不 CONFIRM**(门禁:达成一致前不合 main)

CH 腿主体对 S005/检索件的落地是忠实的(config.d 四键、users.d replace+zz+::/0+整节重写、env 三元组不设、uid 101、reqwest 两面、探针双头),但契约账置空引入了一条 PG 真机回归,resume/建库两臂各有一个机制级漏门。

---

## F1(PG 真机回归,契约账引入):证书面 password 置空 → postgres entrypoint 拒起

**位置**:postgres.rs(start 内 `let password = if tls { String::new() } else { ... }`)+ docker.rs create_postgres 恒设 `POSTGRES_PASSWORD={}`(docker.rs 该函数 env 三元组无条件,见 14fe18c)。

**机制**:本批把 PG 证书面 password 从「生成随机串」改为空串,于是 cert-face fresh start 的容器 env 带 `POSTGRES_PASSWORD=`(空)。postgres 镜像 entrypoint 的 docker_verify_minimum_env:`-z "$POSTGRES_PASSWORD"`(空串即空)且 `POSTGRES_HOST_AUTH_METHOD` 非 trust → 打「Error: Database is uninitialized and superuser password is not specified」**exit 1**(upstream docker-library/postgres 18/trixie docker-entrypoint.sh:102-119 核;该检查只在 PGDATA 未初始化臂,行 104 注释与 347 行调用点实证)。结果:fresh 证书面 start → 容器即死 → 就绪超时 → 回滚报错。**fresh 证书面 PG start 100% 失败**;存量实例 resume 不受影响(PGDATA 已初始化跳过该检查)。

**为何门禁没抓到**:假 Docker 夹具只断言 create 请求形状,不仿真 entrypoint 的密码要求;REQ-0016 的 11/11 实弹是本批之前跑的。

**修法**(二选一):
- (a) 最小:证书面仍 `generate_password()` 进 env(口令惰性,hba cert-only 永不受理),只把**输出面**置空——env 供应与打印解耦,不动 entrypoint 面;
- (b) 证书面加 `POSTGRES_HOST_AUTH_METHOD=trust`:entrypoint/initdb 临时面走 trust,最终 hba 由 `-c hba_file`(dctl cert-only 件)接管,trust 不落终态。注意 (b) 会触发 entrypoint 的 trust 大警告日志。
修后**必须在 lan-linux2 复跑 PG 电池**(scripts/test-postgres-integration.sh)再谈合入。FK/CH 两腿无此坑(redis 无密码 env 要求;CH 证书面 env 三元组整组不设,S005 已证 entrypoint 哈希臂跳过),无需联动改。

## F2(契约未交付):「三引擎 start 输出零口令行」只对 CH 成立

**位置**:output.rs 未在本批改动。PostgresStartOutput(:725-733)与 FalkorStartOutput(:751-758)无 `skip_serializing_if`;两者 Display(:741、:766)无条件 `writeln!("  Password: {}")`。ClickhouseStartOutput 有 skip(:786)且 Display 条件打印(:797-799)。

**现状与申报的差距**:证书面 password 空串后——人类面 PG/FK 打印裸 `  Password: ` 空行(比装饰行更像渲染事故);JSON 面 PG/FK 出 `"password": ""`,CH 不出该键,三引擎 JSON 形反而分裂。提交信息与 ADR-0011 追注的「三引擎统一零口令行」不成立。

**修法**:PG/FK 两 struct 照 CH 补 `#[serde(skip_serializing_if = "String::is_empty")]` + Display 条件打印。顺带:README CH 节残留「随机密码由 start 打印一次」无面限定句(cli.rs CONTEXT 已改成对的口径),一并顺手。

## F3(CH resume 漏门):证书面 resume 必打假口令告警

**位置**:clickhouse.rs resume_existing 尾,`warn_if_credentials_rejected(info.http_port, &user, &_password, &database)` 无面分派。

**机制**:该函数走**明文 http_query**(127.0.0.1:http_port + user/password,clickhouse.rs:803-819)。证书面 8123 是 strict https,明文请求握手即败 → warn 臂必触发,每次证书面 resume 打:「Warning: the server is up but rejected the printed credentials…keeps the password from its first initialization」——证书面根本没打印口令,指引描述的场景也不存在,纯误导。fresh start 路径已正确把它收进 else(口令面)臂,resume 漏了同一扇门。(注:口令面在 loopback 受限 daemon 上也会假警,那是存量;证书面是全环境必发。)

**修法**:tls 臂跳过(一行门),或换 https_query 做证书面探活。

## F4(建库路径半态):证书面 CREATE DATABASE 走宿主发布口,失败无回滚

**位置**:clickhouse.rs start() 就绪成功后 `https_query("127.0.0.1", http_port, …CREATE DATABASE IF NOT EXISTS…).await?`。

**机制两条**:
- (a) 与本批自身设计理由矛盾:证书面就绪探针特意走容器内,注释明写「发布口宿主 loopback 不可达的环境」;建库却走 127.0.0.1:发布口。lan-linux2 无 python 转发器时,`start --database x`(证书面)必败。
- (b) `.await?` 裸传播:容器在跑、元数据已落盘、start 返回 Err——健康批 F-14 消灭的「半态」复活;cli.rs CONTEXT 承诺「A failed fresh start rolls back container and data」在此路径失守;用户再 start 得 ServerAlreadyRunning,与首次报错互相矛盾。

**修法**(候选,择一即可):建库改容器内 exec(与探针同理全环境可达,推荐);或失败降级为 warning(口令面 warn_if_credentials_rejected 先例);或失败入回滚。无论哪条,把不变量对齐。

## F5(静默错库):证书面 --database 只建库不设默认,client/dotenv 回落 "default"

**机制**:口令面经 CLICKHOUSE_DB env 记住启动库(read_ch_env),证书面按 S005 不设该 env(entrypoint 会拉明文 init 客户端),ServerInfo 又无 database 字段(server.rs:56-85)。于是 `server start --database events`(证书面)建库成功,但后续 `client -q` 与 `dotenv` 的数据库默认是 read_ch_env 缺省 DEFAULT_DATABASE="default"——口令面同命令落在 events。**查询/写入静默落错库**,且 dotenv 证书面写出的 `CLICKHOUSE_DATABASE=default` 是错值。

**修法**(需裁,二选一):ServerInfo 增 database 字段(serde default 兼容旧元数据,resume/dotenv/client 三路改读元数据);或明文裁定「证书面 --database 只建不设默认」并在 start 帮助与 dotenv 输出写明。静默现状不可接受。

---

## G(建议,采纳与否留白)

- **G1 resume native 口刷新键随面**:clickhouse.rs resume_existing 以 "9000/tcp" 刷 tcp_port,证书面容器绑定 "9440/tcp" → 刷新落空退回 prior;正常 resume 无碍,孤儿恢复(tcp_port=0)后则永远 0(dotenv CLICKHOUSE_PORT=0、list 显示 0)。修法:`if tls { "9440/tcp" } else { "9000/tcp" }`。
- **G2 ServerInfo.tls 契约注释过期**:server.rs:79-82 仍写「Postgres authentication face…Other engines leave it None」,FK 批起三方在用。/// 契约注释是仓规真相源,顺手改。
- **G3 client CONTEXT 直连面口径**:cli.rs client 块「dctl-managed instances do(require auth)」对证书面托管实例不成立(直连 --user/--password 打 strict https 必败);直连说明可加「证书面实例用受管模式」一句。
- **G4 ca fake-home 论证今日成立但无护栏**:unsafe set_var 后 HOME 不还,scratch tempdir drop 后 HOME 指向已删目录;我核了全 src 单测面(skills.rs:1032 等用 temp_test_dir 显式传参,不读 HOME env),「唯一 HOME-touching 单测」当前属实——但这是活契约,后续任何读 HOME 的单测都会隐形竞态。测试基建批上 serial 锁或 restore 护栏时一并收。
- **G5 wget 在 CH 镜像内的存在性未实证**:容器内探针全新依赖 wget,S005 未记录该点,CH 电池未跑(REQ 实弹待)。合前电池首跑即证;若镜像无 wget,探针永不 ready——归入「合前必跑电池」门槛,不单列 F。

## 申报七问逐项裁决

1. **S005 忠实落地:过**。users.d replace 整节 + 全项重写(networks ::/0/profile/quota/access_management)+ zz- 排序 ✓;config.d 明文口挪 9400/9500 不删 ✓;openSSL/server 五键 strict + loadDefaultCAFile false ✓;env 三元组证书面不设 ✓(recover 以 CLICKHOUSE_PASSWORD 缺席推面,与 create 不变量互洽);CN 恒 default + named user 用法错 ✓。
2. **https_query:过**。tls_certs_only(非 deprecated add_root_certificate)+ Identity::from_pem(key+cert 拼接)+ 双认证头,URL 无 user/password(database query 参数不在禁令内)✓ 与检索件第 7 节逐点一致;传输错误信封自撰句 + stderr 库文,非 2xx 走既有 ClickhouseHttp redacted 臂,与口令面同构 ✓。
3. **回滚区:签发/上传/start_existing 三路径完备;建库路径不完备(F4)**;CREATE DATABASE 时机(ready 后、输出前)本身正确。
4. **readiness 双面分派:过**。证书面 poll 循环(Exited→ReadinessFailure::Exited、Running→探针、Err→记 last_probe_error 续 poll、timeout→TimedOut)与口令面语义对齐;口令面 host 探针不变 ✓。
5. **契约账三件:upload-500 方向正确过**(信封行无 daemon 文、stderr 有人话、DELETE 回滚与元数据清除双断言);**password 置空未真交付(F2)且引入 F1**;**ca fake-home 今日安全(G4)**。
6. **夹具:过**。tls/upload_status/readiness_exit_codes 三旋钮语义正确,exec 应答链(create→start→inspect json 退出码回读)仿真到位;旧用例钉 --auth password 后原断言语义不变;cert 新用例钉了 env 空、双发布键、tar 八文件、探针 argv、元数据 tls=true。
7. **文档口径:基本过**。README CH 节/行 8-9、ADR-0011 收官追注、REQ 勾选(CH 实弹未勾,诚实)与码一致;例外 = ADR 追注「统一零口令行」被 F2 证伪、README 残留句见 F2 顺带项。

## 合前门槛(我的放行条件)

1. F1-F5 处置达成一致;
2. lan-linux2 复跑:CH 电池首跑(兼证 G5 wget 在否)+ **PG 电池复跑**(F1 修复实证);FK 电池 10/10 已在档无需复跑。
