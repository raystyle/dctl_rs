---
id: REQ-005
title: registry.ohmygh.com 私仓直连与离线回落
status: implemented
priority: could
trace: registry-fallback 批(ADR-0008;crates/databasectl/src/local/registry.rs;tests/local_registry_test.rs)
---

> 总台 OCI 集成边界裁定登记(2026-09-20,插队轻量单,零改动回执合法);归 roadmap 裁量。

# registry 私仓直连与离线回落

## Scenario

端上无 omc 二进制时,dctl 以原生 Rust HTTP 客户端直连 registry.ohmygh.com(registry:2,v2 协议,basic 凭据从本地密档读),拉取常用数据库镜像;上游不可达时按回落序工作。

## Criteria

- [x] registry v2 API 客户端:/v2/_catalog、/v2/<name>/manifests、blobs;basic 凭据本地密档,零入仓零 argv
- [x] 离线回落序:Docker Hub 上游直连到 registry.ohmygh.com 到本地缓存;常用镜像 latest 锚预置清单向总台提请(锚清单仍在总台侧,不阻塞实现面)
- [x] docker-load tar 通道作端间转移兜底,语义复用不耦合代码;层遍历规范文档按需向总台 REQ 提请(格式由 ADR-0008 裁定为 OCI image layout,非 docker-save 私有格式:规范公开且 docker load 原生接受)
- [x] 与现有 Docker 引擎(install/start)的接入面与凭据安全边界另立 ADR 后动工(ADR-0008)

## 实现追注(2026-09-21)

- 落地为 `dctl local registry pull/catalog` 显式命令加 install/start 透明回落链;测试面 stub v2 registry + fake docker 端到端,真 Docker 复验走 lan-linux 配方(docs/diary/2026-09-20-fork-bootstrap.md)。
- 私仓 repo 命名约定(Hub 风格 org/name 是否原样托管)与 latest 锚清单归总台裁定;若需别名映射另立需求。(2026-09-21 下午总台已裁定:Hub 原名托管零映射、建锚清单机制,草案 postgres:18、falkordb/falkordb:v4.20.6、clickhouse/clickhouse-server:26.8;三引擎镜像待补种进私仓)
- lan-linux 真 Docker(Docker 29.8.1,containerd 镜像存储)实证:docker load 按 layout 的 ref.name 注记**字面**起名,而引用解析按归一名(docker.io/library/postgres)匹配;注记写全限定名后才可被 run/create 解析。classic 存储无此坑,属 containerd 存储特有。
