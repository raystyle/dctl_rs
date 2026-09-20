# REQ 索引

> 需求登记索引;新建拷 `docs/requirements/REQ-000-template.md`,编号三位接当前最大号,退役不复用。状态:draft 到 implemented(回填 trace)或 rejected。

| id | 状态 | 优先级 | 标题 | trace |
| --- | --- | --- | --- | --- |
| REQ-001 | draft | should | Engine 抽象泛化,接入第三引擎 | null |
| REQ-002 | draft | should | 物理删除 telemetry.rs 与 failure.rs | null |
| REQ-003 | implemented | must | FalkorDB 图数据库引擎接入 | cargo test -p databasectl --test local_falkor_readiness_test |
| REQ-004 | draft | should | 集成 ledger 标准的 issue 与 artifact 命令族 | null |
| REQ-005 | draft | could | registry.ohmygh.com 私仓直连与离线回落 | null |
