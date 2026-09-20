---
id: ADR-0002
title: 命名与状态目录:仓库 dctl_rs、crate databasectl、二进制 dctl、状态目录 .dctl
status: accepted
date: 2026-09-20
deciders: [ray]
supersedes: []
superseded_by: null
tags: [naming, identity]
---

# 命名与状态目录

## Context

fork 需要与上游身份隔离,同时不与已安装的 chctl 状态冲突。上游模式是 crate clickhousectl 加二进制 clickhousectl 与 chctl 软链;ClickHouse Control 的定位对我们收窄为 DataBase Control。dctl 在 crates.io 未被占用 [实证: 2026-09-20 crates.io API 返回 404],但我们暂不发布 registry。

## Decision

仓库 `raystyle/dctl_rs`(private);crate 目录与包名 `databasectl`,二进制名 `dctl`;状态目录从 `.clickhouse` 改为项目级 `.dctl/` 与全局 `~/.dctl/`,与 chctl 安装完全隔离。保留不改:builds.clickhouse.com 与 packages.clickhouse.com 产品下载 URL、docker 镜像名、`~/.local/bin/clickhouse` 符号链接(它暴露的是 ClickHouse 产品二进制,不是 ctl 工具)。改名清扫整体入 `.git-blame-ignore-revs`,blame 仍指向上游源头。

## Consequences

- 好面:命令行敲起来短(对标 chctl);状态隔离后 dctl 与 chctl 可并存;blame 借 ignoreRevs 保持可追溯 [实证: 2026-09-20 blame-ignore 登记后 git blame 抽查]
- 坏面:backport 时每一处身份字符串都要人工映射,重命名冲突无法机械合并;用户从 chctl 迁移时已装的 ClickHouse 版本不会被 dctl 复用,需重新 install
- 坏面:产品词 clickhouse 与工具身份 dctl 在代码里共存,清扫规则必须靠残余 grep 门禁维持(AGENTS Must not)
