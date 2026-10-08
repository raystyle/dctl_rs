# REQ-0017 CH 腿二轮快核回执

基线: `14fe18c`. 提交: `6f7d175`, `f81825a` (`14fe18c..f81825a`, 14 文件). 未重跑门禁, 未复跑电池. 请求里的 369/0 与双 clippy 按声明收, 不复验.

结论: 不放行. 3F.

## 一轮已收口

1. 上一轮 F1: resume 的 native 键在 `prior.tls == Some(true)` 时取 `9440/tcp`, 否则 `9000/tcp` (`clickhouse.rs` 758 到 765 行). list 次端口候选是 `[9440, 9000]`, 先中先得 (`docker.rs` 1693 到 1708 行). 夹具 inspect 在 `tls` 时渲染 `9440/tcp`. `tls: None` 的旧元数据仍走 9000, 口令面告警仍在.
2. 上一轮 F2: `warn_if_credentials_rejected` 只在 `prior.tls != Some(true)` 时调用 (`clickhouse.rs` 809 行).
3. 上一轮 F3 的新鲜 start: `PostgresStartOutput` 与 `FalkorStartOutput` 空 password 跳过序列化, Display 只在非空时写 Password 行 (`output.rs` 733 到 776 行). FK 证书面 resume 从 `REDIS_ARGS` 解析, 没有 `--requirepass` 时是空串, 行不出. PG 的 resume 见下方 F2.
4. 上一轮 F4 的回滚臂: 非 default 库的 CREATE 失败会拿 metadata 锁并调用 `rollback_failed_fresh_start` (`clickhouse.rs` 494 到 504 行). 查询改走容器内 `clickhouse-client --secure --config`, 不再打宿主发布口. 失败怎么报, 见下方 F1.
5. 上一轮 F5 的完整元数据路径: 证书面 start 把请求的库写入 `ServerInfo.database` (`clickhouse.rs` 401 行). client 与 dotenv 在字段为 Some 时用它, 不再只信缺席的 `CLICKHOUSE_DB` (`clickhouse.rs` 1432 与 1653 行). resume 的输出和孤儿恢复见下方 F3.
6. PG entrypoint: 证书面容器 env 仍写入生成的 `POSTGRES_PASSWORD` (`postgres.rs` 394 到 415 行, `docker.rs` 499 行), 新鲜 start 的输出 password 是空串. hba 仍是 `hostssl cert` 加 `host reject` (`docker.rs` 454 到 459 行). resume 把这串印出来, 见下方 F2.
7. G1 不采纳已记在 REQ-0017 实弹勾选之后: 宿主客户端是发布口不通的第一发现人. 不复开.
8. G2: `ca.rs` 单测在 `issue_server_cert` 之后 `remove_var` 并写回原来的 HOME (202 到 210 行).
9. G3: README 口令面一句已限定到口令面, 证书面写零口令. client CONTEXT 直连句改为证书面实例走受管模式. 该块 8 行内容, 未超上限.

## F1 `clickhouse_tls_query` 把客户端字节写进进程 stdout, 失败被涂成笼统的 Docker 错误; 读不到退出码就当建库成功

`docker.rs` `clickhouse_tls_query` (850 到 926 行) 把 clickhouse-client 的 stdout 和 stderr 都 attach. 每个 chunk 经 `into_bytes` 后写入 `tokio::io::stdout()` (899 到 908 行). 调用方丢掉返回值 (`.map(|_| ())`), 这些字节的唯一去处是 start 的 stdout. 就绪探针是反例: `attach_stdout: false`, 只读退出码 (`docker.rs` 950 行).

失败时客户端的异常文本在它自己的 stderr 上. 这段文本被拷进进程 stdout, 然后函数返回 `Error::DockerError`, 正文是 `clickhouse-client exited with status {code}` (917 到 923 行). 信封对 `Error::DockerError` 整臂替换成 `Docker operation failed` (`output.rs` 298 到 299 行), 人类文本走 stderr. `--json` 的 stdout 因此不是一个 JSON 对象, 而是客户端原文; stderr 的 message 既没有退出码, 也没有自撰的补救句. upload-500 的契约是外来文本只在 stderr 的人读行, 信封只有自撰句. 这条新路径把外来正文放上了机器通道, 又把自撰的状态句塞进会被涂掉的变体.

同一函数的状态判断在读不到码时当成成功: 只有 inspect 成功且 `exit_code` 为 Some 且非 0 才返回 Err. inspect 本身失败, 或 `exit_code` 缺席, 都落到 `Ok(result)` (917 到 925 行). start 随之报成功. resume 不建库. 这是上一轮 F4 要消灭的半成品, 留在「状态没读到」这条臂上. `StartExecResults::Detached => Ok("")` (896 行) 在本调用的 attach 配置下不应出现, 不单列.

## F2 PG 证书面的第二次 start 打印惰性口令

新鲜证书面把生成口令放进容器 env, 输出结构里的 password 置空 (`postgres.rs` 394 到 401 行). `resume_existing` 无条件 `read_pg_env`, 把 `POSTGRES_PASSWORD` 放进 `PostgresStartOutput` (808 到 818 行). 证书面这串非空, Display 就写 Password 行, JSON 也不再跳过. dotenv 证书面不写这串 (1555 到 1567 行), 与 start 的第二次输出不一致.

hba 是 `hostssl all all all cert` 与 `host all all all reject` (`docker.rs` 458 到 459 行). 印出来的口令不能用于发布口. 上一轮的契约是证书面 start 不再打印无效 Password 行. 第一次 start 守住了, 同命令的 resume 把刚藏起来的口令打出来.

## F3 证书面的库名没有进 resume 的输出, 孤儿恢复写成 None

上一轮 F5 的修法是 resume, dotenv, client 三路改读元数据. dotenv 与 client 读了. resume 的输出没有.

`resume_existing` 的 `database` 来自 `read_ch_env` (742 行). 证书面没有 `CLICKHOUSE_DB`, 该函数落到 `DEFAULT_DATABASE` (1360 行). 输出用的就是这个值 (821 行). `ServerInfo` 用 `..prior` 保住了字段 (772 到 777 行), 所以这次 resume 之后 client 和 dotenv 仍对. 同一次 start 的 JSON `database` 却是 `default`. 口令面 resume 测试把这个字段钉成容器 env 里的库 (`local_clickhouse_docker_test.rs` 1148 到 1151 行). 证书面按同一字段会报成 default, 而新鲜 start 的 JSON 写的是请求的库. 第二次 start 改口.

孤儿恢复是另一处作者. `recover_project_clickhouse_blocking` 在元数据缺失时写 `database: None` (`docker.rs` 2260 到 2271 行). 库名不在 env, 也不在 label (create 的 label 只有 engine, name, major, project, created-by, 1286 到 1291 行). 恢复后的 client 和 dotenv 走 `unwrap_or`, 又落到 `default`, 数据目录里 CREATE 出来的库还在. 这是上一轮 F5 的原症状, 留在本批刚改过的恢复作者上. 口令面不受影响: 那边 `None` 会退回 `CLICKHOUSE_DB`. 静默退回 default 是缺陷; 恢复时不知道库名就该报出来, 而不是当成 default.

## 不另立

- README 第 16 行快速示例仍写 postgres start 打印生成的密码. 不在本 diff. 同文件 146 行已经按面分开. 不升 F.
- `CREATE DATABASE IF NOT EXISTS {database}` 仍是未加引号的插值, 经 argv 进 clickhouse-client, 不经 shell. 与上一轮的宿主查询和镜像 entrypoint 同一类, 不新立.
- REQ 实弹勾选仍是未做. 不把脚本存在当成已跑.
