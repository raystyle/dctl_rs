# REQ-0017 CH 腿二轮快核(kimi 腿,修复批复核)

- 增量:14fe18c..f81825a 两笔(6f7d175 grok 腿 + f81825a kimi 腿),对本地提交取证(SHA 钉死内容)
- ⚠️ **流程异常报备**:两笔截至核时在本地对象库而不在远端——`git ls-remote origin feat/req0017-ch-mtls` 仍是 14fe18c,「已推送」不成立(首次 fetch 遇上 GnuTLS 断流,重试成功后可证)。按 SHA 取证不受影响,但若合入前 rebase/amend 改了 SHA,本回执作废需重核。
- 门禁本机复跑(f81825a):369/0/1,fmt 净,双 clippy 0 error,check-md 53 净,分类器 OK(与申报一致)
- 结论:**新 F×1(琐碎必修),暂不 CONFIRM**;首轮 5F+5G 其余处置全部忠实,无机制级新缺陷

## 首轮 5F 处置核对

- **F1(PG entrypoint 空密码)**:采修法 (a) 忠实——`provisioned_password`(env/旗标/生成)恒进 PostgresRunOpts,输出面 `password` 证书面置空;口令面行为逐字不变。注释写明「hba 永不接受 = 惰性凭证」理由。✓
- **F2(输出行)**:PG/FK 补 `skip_serializing_if` + Display 条件打印,与 CH 同形;三引擎两面(人类/JSON)现一致。✓
- **F3(resume 假警告)**:tls 臂跳过(`if prior.tls != Some(true)`),口令面不变。✓
- **F4(建库)**:两臂都修——(a) 改 docker.rs `clickhouse_tls_query`(容器内 clickhouse-client --secure --port 9440 --config cli.xml,发布口解耦与探针同理);(b) 失败经 `bootstrap` Option 入 `rollback_failed_fresh_start`,与就绪失败同路径。✓
- **F5(静默错库)**:ServerInfo 增 `database: Option<String>`(serde default + skip none,旧元数据兼容);CH 证书面存 Some(启动库),client() `info.database.unwrap_or(env_db)`、dotenv 同优先,resume 沿 prior,recover None。PG/FK/各测试构造点全补 None,编译面齐。✓

## 首轮 5G 处置核对

- G1 native 键:list 双候选 [9440,9000](9440 优先)+ resume 按面选键 + 夹具 inspect 按面出绑定。✓
- G2 契约注释:**修了但没修对,见新 F1**。
- G3 CONTEXT/README:直连面加「certificate-face instances need managed mode instead」;README 残句改「口令面的随机密码…证书面零口令」。✓
- G4 HOME 还原:swap 后 remove_var + 重设 original,护栏成立。✓
- G5 wget:S005 探针容器内 wget 实证在档;CH 电池首跑在途(见放行条件)。

## 新发现

### F1(必修,琐碎):G2 的修复把 tls 契约注释改成病句

server.rs:79-84 现文:
```
/// Postgres authentication face (ADR-0011): `Some(true)` = certificate
/// (mTLS) instances, `Some(false)` = explicit `--auth password`, and
/// Authentication face: `Some(true)` certificate mTLS, `Some(false)`
/// password, `None` = metadata predating the face split (password
/// behavior).
```
旧句前段没删干净:「Postgres authentication face (ADR-0011): … and」悬空后接新句,既仍误标 Postgres-only(G2 的靶子原样还在),又是半截语法。/// 契约注释是仓规真相源,且本轮台账宣称 G2 已修——宣称与实态不符属必修类。修法:删掉前两行旧残段,留新句(可补「三引擎共用」)。

### G(留白)

- **G1 --database 裸插 SQL**:`CREATE DATABASE IF NOT EXISTS {database}` 无标识符校验(14fe18c 即有,我首轮漏网,非本修复引入)。本地单用户工具、自指向,风险低;但带空格/反引号的笔误会以服务器语法错+回滚收场,建议在 validate_start_options 加标识符白名单(`[A-Za-z_][A-Za-z0-9_]*` 或 CH 反引号规则)或直接拒绝非常规字符。归测试加固/契约批均可。
- **G2 clickhouse_tls_query 把 exec 输出 tee 到宿主 stdout**:今日唯一调用(CREATE DATABASE)输出为空故无害;helper 注释已自限用途。未来复用时注意 --json 面污染。记档即可。

## 放行条件(维持首轮 + 本轮状态)

1. ~~首轮 5F~~ 已清;**本轮 F1(注释病句)修掉**;
2. 实弹两道:CH 电池首跑(兼证 wget 在否/容器内建库),PG 电池复跑(F1 修复实证)——工位已在途,结果回报后我这里无追加审查需求,直接由你方凭证归档;
3. 推远端:6f7d175/f81825a 尚未上 origin(见报备),推送后若 SHA 不变无需复核。

静态面除 F1 外全清;F1 是一行级,修后即可当我 CONFIRM(快核纪律:下轮只核注释一笔,若只动它)。
