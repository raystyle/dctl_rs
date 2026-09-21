# Changelog

## 未发布

- **ledger 权限收口(REQ-006,总台统一裁)**:`dctl ledger` 移除 `issue close`、`artifact promote` 与 attest 的 `promote/demote/supersede` 类别——本 CLI 只增 issue 与产物及验证类 attest;关闭与删除唯一道走 omc 工位(`omc ledger issue status <repo> <n> <to>` / `omc ledger issue delete`)。自研签名道退役,改为共享 ledger-client crate(v0.1.1)。

## v0.6.0

- ClickHouse Docker 化:三引擎统一容器生命周期,client 内置集成(查询走 HTTP,交互走 docker exec);二进制版本管理退役(ADR-0007)。
- FalkorDB 图数据库引擎(PR #1);ledger issue/artifact 命令族(PR #3);家族幂等键标准(PR #4)。
