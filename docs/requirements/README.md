# REQ 索引

> 需求登记索引;新建拷 `docs/requirements/REQ-000-template.md`,编号三位接当前最大号,退役不复用。状态:draft 到 implemented(回填 trace)或 rejected。

| id | 状态 | 优先级 | 标题 | trace |
| --- | --- | --- | --- | --- |
| REQ-001 | implemented | should | Engine 抽象泛化,接入第三引擎 | 第三引擎由 FalkorDB 批(PR #1)满足;四引擎不做(2026-09-21 裁定) |
| REQ-002 | implemented | should | 物理删除 telemetry.rs 与 failure.rs | PR #8;全仓 telemetry 引用清零 |
| REQ-003 | implemented | must | FalkorDB 图数据库引擎接入 | cargo test -p databasectl --test local_falkor_readiness_test |
| REQ-004 | implemented | should | 集成 ledger 标准的 issue 与 artifact 命令族 | cargo test -p databasectl --test ledger_request_test |
| REQ-005 | implemented | could | registry.ohmygh.com 私仓直连与离线回落 | registry-fallback 批;ADR-0008(格式裁定 OCI layout) |
| REQ-006 | implemented | must | 迁移到共享 ledger-rs crate 并统一权限收口 | PR #7;ledger-client v0.1.1 |
| REQ-007 | implemented | must | ClickHouse 引擎 Docker 化,三引擎统一容器管理 | PR #6;ADR-0007 |
| REQ-008 | implemented | should | Postgres 与 FalkorDB 原生客户端集成(全引擎宿主免装客户端) | native-clients 批;ADR-0009 |
| REQ-010 | draft | should | 镜像拉取默认走私仓并可指定自定义仓库 | null |
| REQ-011 | draft | must | 去掉 local 命令前缀层,默认即本地操作 | null |
| REQ-009 | draft | should | 三引擎免密密钥身份认证(客户端证书 mTLS) | ADR-0011 |
| REQ-012 | implemented | must | clickhouse start 增 --bind 旗标,端口可发布到非 loopback 面 | --bind 批 f7a39d2;NSM 部署真机验收 |
| REQ-013 | implemented | should | skills 面剪枝:只装与 dctl 命令域对齐的引擎知识项 | 剪枝批 4e6834f;实机冒烟收敛两项 |
