---
id: ADR-0005
title: 接入 FalkorDB 图数据库引擎,Docker 托管全功能镜像
status: accepted
date: 2026-09-20
deciders: [ray]
supersedes: []
superseded_by: null
tags: [engine, docker, falkordb]
---

# 接入 FalkorDB 图数据库引擎

## Context

REQ-001 要求 Engine 泛化并接入第三引擎;用户裁定首个第三引擎为 FalkorDB(Redis 模块图数据库,RedisGraph 后继,openCypher 接口)。备选形态:官方二进制直管(FalkorDB 官方只发 Docker 镜像,无二进制 tarball)对应 Docker 托管。镜像有两族:`falkordb/falkordb`(含 Browser Web UI,双端口 6379+3000)与 `falkordb/falkordb-server`(无浏览器,官方建议生产用)。版本标签只有全量 vX.Y.Z 与 latest,无 major 游动标签 [实证: 2026-09-20 Docker Hub tags API];镜像内自带 redis-cli 8.6.3 [实证: 2026-09-20 lan-linux 实测 which redis-cli];数据路径 /var/lib/falkordb/data;认证走 REDIS_ARGS 的 requirepass,模块调优走 FALKORDB_ARGS [实证: 官方配置文档]。FalkorDB 服务端为 SSPLv1,但 dctl 仅拉取镜像并编排容器,不链接其代码,Apache-2.0 不受影响。

## Decision

以 Docker 托管接入,镜像用 `falkordb/falkordb` 全功能版(Browser 是本地图可视化的加分项,server 轻量镜像留作后续变体);子命令族 `local falkordb start/stop/stop-all/remove/client/dotenv` 完全仿 postgres 形制;实例键 `<name>-fk<version>`(版本轴用全量 X.Y.Z,因无游动标签),默认钉 4.20.6;端口 6379 协议口加 3000 Browser 口,同 +100 自动挑口策略;认证默认随机 24 字符密码经 REDIS_ARGS 注入,resume 从容器 env 回读;就绪探针为容器内 exec `redis-cli -a <pw> ping` 等 PONG;client 宿主 redis-cli 优先、docker exec 回退;-q 直通 redis 命令不做包装。泛化边界:仅泛化字符串判定(label 过滤、输出表 has_docker_engine),容器创建与探针平行新建 fk_* 函数,不动 pg_instance_key 家族(REQ-001 的完整抽象重构另行推进)。

## Consequences

- 好面:第三引擎按既有 Docker 引擎模式接入,postgres 双引擎零回归风险可控;Browser 端口开箱即用
- 坏面:版本默认钉死具体补丁号,上游发新版需显式升级(无 major 游动标签可跟);falkordb 与 postgres 两套 Docker 引擎代码并存,REQ-001 的抽象重构欠账加深了一层
- 坏面:双端口意味着端口冲突面更大(6379 与常见 Redis 撞口),自动挑口策略必须两口都生效
- 注意:SSPLv1 限服务端分发;dctl 若未来分发内置 FalkorDB 的组合产物需重审,当前纯编排无碍
