# REQ-0017 CH 腿三轮快核回执

基线: `f81825a`. 提交: `a3560f3` (`f81825a..a3560f3`, 7 文件). 未重跑门禁, 未复跑电池. 请求里的 370/0、CH 电池 10/10、PG 电池 14/16 按声明收, 不复验.

结论: 不放行. 2F + 1G.

## 二轮已收口

1. F1 的探针形: `clickhouse_tls_query` 现在 `attach_stdout/stderr/stdin` 全 false, `detach: true` 启动, 然后轮询 `inspect_exec` (`docker.rs` 874 到 931 行). `exit_code` 为 `Some(0)` 返回 Ok. 非 0 时 stderr 打状态码, 信封是 `Error::ClickhouseUsage` 自撰句 (912 到 918 行). `exit_code` 为 None, 以及 inspect 失败, 都返回 Err, 不再当成成功 (901 到 928 行); inspect 失败的 daemon 文本只在 stderr. 调用点是 `.await` 的 `Result`, 不再 `map` 丢掉 (`clickhouse.rs` 495 到 511 行), Err 仍进 `rollback_failed_fresh_start`. 超时臂的变体见 G1.
2. F2: PG `resume_existing` 在 `prior.tls == Some(true)` 时把输出 password 置成空串, 再放进 `PostgresStartOutput` (`postgres.rs` 808 到 825 行). 空串走已有的跳过序列化与条件 Display. `tls: None` 与口令面仍印 env 里的口令. dotenv 证书面本来就不写这串.
3. F3 的 resume 输出: `prior.database.clone().unwrap_or(env)` (`clickhouse.rs` 759 到 763 行), 该值进入 start JSON (842 行). 证书面新鲜 start 存过 `Some`, 第二次 start 不再把 JSON `database` 改成 env 的 default. 口令面 `prior.database` 为 None, 仍退回 `CLICKHOUSE_DB`. 恢复路径的注记见 F1.
4. kimi 二轮的注释病句: `ServerInfo.tls` 现为完整的一句, 三引擎共用, `None` 是分面之前的口令行为 (`server.rs` 79 到 81 行). 旧残段不在了.
5. kimi 二轮 G1: `validate_start_options` 拒绝空串、非 ASCII 字母数字与下划线之外的字符、以及以数字开头的 `--database` (`clickhouse.rs` 158 到 170 行). 错误是 `ClickhouseUsage`. 单测覆盖空串、空格、分号、前导数字、连字符, 并放行 `events_2` (1901 到 1934 行). 白名单之后, 未加引号的 `CREATE DATABASE IF NOT EXISTS {database}` 不再吃到自由文本.
6. 上一轮不另立的 README 第 16 行与实弹勾选, 本 diff 没动. 电池脚本把带空格的认证头收成函数参数, 口令面在「did not become ready」时记一笔环境说明并返回 0. 与申报的 SKIP 一致, 未复跑.

## F1 恢复注记在用户已经指定 `--database` 时仍说正在查 default

`client` 在 `tls && info.database.is_none()` 时无条件 eprintln: 正在查 default, 请改传 `--database` (`clickhouse.rs` 1454 到 1461 行). 真正使用的库是 `database.as_deref().or(Some(db))` (1474 行, 交互腿 1508 行). 旗标优先于元数据和 env.

恢复实例上, 用户按注记补了 `--database events` 之后, stderr 仍写「querying the default database」, 查询和交互会话却进 events. 这条注记是上一轮 F3 用来消除静默的全部人话, 它推荐的下一步会让它自己说错. 注记应只在旗标缺席时出现; 旗标已经给了库名, 就不要再声称落在 default.

不带旗标的恢复实例, 注记与实际的 default 一致, 这一支没问题.

## F2 恢复后的 dotenv 仍静默写成 default

上一轮 F3 点名的两条消费者是 client 和 dotenv. client 有注记 (见 F1). dotenv 没有.

`dotenv` 只在 `info.database` 为 Some 时覆盖 env 读到的库名 (`clickhouse.rs` 1680 到 1685 行). 恢复写的是 None, 证书面又没有 `CLICKHOUSE_DB`, 于是 `CLICKHOUSE_DATABASE` 写成 default, 成功退出, 无人话. 只读 dotenv、不跑 `client` 的一侧仍是上一轮那个静默错库: 数据目录里 CREATE 出来的库还在, 写出去的文件说是 default.

## G1 探针超时和「意外 attach」仍走会被涂掉的 DockerError

采纳:

不采纳:

退出码与 inspect 失败已经是 `ClickhouseUsage`, `--json` 能看到自撰句. 同函数里还有两臂把自撰句放进 `Error::DockerError`: 意外 attach (895 到 898 行), 以及 3 秒轮询耗尽 (932 到 934 行). 信封对 `DockerError` 整臂替换成 `Docker operation failed`. 人读 Display 仍有超时句, 命令也是失败并回滚, 不是半成品. 建议这两臂与旁边的 `ClickhouseUsage` 对齐, 让 `--json` 看得到「没在时限内结束 / 已回滚」, 而不是一条笼统的 Docker 失败. 3 秒上限本身本轮不单列: 建库用例已在申报的电池里通过.

## 不另立

- 非法 `--database` 在 resume 之前就被 preflight 拒绝. resume 仍忽略合法的该旗标. 失败关闭, 不升 F.
- 探针形不采集 clickhouse-client 的异常正文, stderr 只有状态码. 与本轮申报的 detach 形一致, 不把「看不到服务器原文」再立成缺陷.
- PG 电池两败按申报当作宿主连接类, 证书面按申报全绿. 未复跑, 不另立.
