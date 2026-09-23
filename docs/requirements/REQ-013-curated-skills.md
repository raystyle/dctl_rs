---
id: REQ-013
title: skills 面剪枝:只装与 dctl 命令域对齐的引擎知识项
status: draft
priority: should
trace: null
---

# skills 面剪枝

## Scenario

用户令(2026-09-23):`dctl skills` 装的是上游 agent-skills 全量包(11 项),其中多数与 dctl 命令面无关,要求修正对齐、剔除无关项。逐项定性:infra-clickhouse/infra-postgres 教的是上游 `clickhousectl` 命令加 ClickHouse Cloud 编排(命令面错、云面无关);clickhouse-managed-postgres-rca 是 api.clickhouse.cloud 云 API;chdb-datastore/chdb-sql 是嵌入式 chdb 引擎;clickhouse-js-node-* 是 Node SDK;clickstack-otel-collector 是托管 ClickStack 接线。保留 clickhouse-architecture-advisor 与 clickhouse-best-practices(纯引擎知识,服务于本仓自管 CH 面)。

## Criteria

- [ ] 保留清单常量(2 项):collect_skill_files 只收集保留项,InstallResult.skills 随之收敛
- [ ] 装后清理:agent 技能目录里「来自上游档案且不在保留集」的既有目录删除,收敛到剪枝后集合;用户自有技能目录(不在上游档案者)不动
- [ ] 非-json 输出行带 pruned 计数;JSON 面按 agent 增 pruned_skills 字段(加法不改旧键)
- [ ] 帮助文本与 README 措辞同步(curated 口径,不钉数量)
- [ ] 单测:收集过滤、清理三分支(保留/上游陈旧/用户自有)、目录缺失 no-op

## 非目标

- 上游包内容本地化(改写生成物违反 Must not;skill 包下载 URL 不动)
- 保留清单可配置化(YAGNI;清单变化走码批)

## 验收判据

- 双 clippy 零警告、fmt 过、全量测试绿(计数随新增测试增加)
- 实机 `dctl skills --all`:skills 列表 = 2 项,五 agent 目录各剔除 9 个陈旧目录,`.claude/skills` 等目录仅余保留项
