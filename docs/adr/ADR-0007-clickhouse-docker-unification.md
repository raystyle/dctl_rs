---
id: ADR-0007
title: ClickHouse 切 Docker 容器管理,三引擎统一,client 在 dctl 内集成
status: accepted
date: 2026-09-21
deciders: [ray]
supersedes: []
superseded_by: null
tags: [engine, docker, clickhouse, architecture]
---

# ClickHouse Docker 化与三引擎统一

## Context

上游用二进制进程管理 ClickHouse(version_manager 下载 + spawn watchdog + SIGTERM/SIGKILL),与 Postgres/FalkorDB 的 Docker 容器生命周期割裂。用户裁定三引擎统一走 Docker,client 在 dctl 内集成(不依赖宿主安装 CLI 工具)。clickhouse/clickhouse-server 镜像有 4 段全量 tag(与现有 VersionSpec::Exact 一一对应)、minor tag、latest;容器内自带 clickhouse-client;HTTP 接口(8123)支持 POST body = SQL 轻量查询,reqwest 已在依赖中。

## Decision

ClickHouse 走与 Postgres/FalkorDB 完全同构的容器生命周期(ClickhouseRunOpts 经 create_clickhouse 经 start_existing 经 /ping 就绪到 label 发现);版本轴 = 镜像 tag(默认钉 26.8 minor 游动标签);端口 8123+9000 双绑定;认证 CLICKHOUSE_* env(默认随机密码);配置 overlay 走单文件 ro bind mount 到 config.d/;readiness 用宿主侧 GET /ping(三引擎中唯一可宿主探测的,现有探针核心原样复用)。client 集成:-q/--queries-file 走 HTTP POST(body = SQL,零新增依赖);交互走 docker exec TTY;直连走 HTTP。整体退役 version_manager(~3842 行)、discovery(~706 行)、symlink(~347 行)及 flate2/tar 依赖。

## Consequences

- 好面:三引擎一套生命周期模式(蓝本复用度极高),代码量净减约 4900 行源码 + 6500 行测试;不再需要 builds.clickhouse.com 二进制下载;进程信号管理(watchdog/SIGKILL 父子对)整体消失
- 好面:client 零宿主依赖(HTTP 内置 + docker exec 回退),-q 模式比 exec 二进制更快(无进程创建开销)
- 坏面:ClickHouse 用户必须装 Docker(原二进制模式可裸跑);local use/which/remove 等版本管理命令消失(镜像由 Docker 管理,无 default 概念);--foreground 交互模式失去对应物(降级为 docker attach 或删除)
- 坏面:stable/lts channel 概念丢失(镜像侧无对应物);旧 ~/.dctl/versions 的存量二进制作废(数据目录兼容但版本管理断代)
- 坏面:配 overlay 单文件挂载意味着 config.d 下只能有一个 dctl 管理文件(多个 config 需合并);ulimit 需显式设 262144
