---
id: REQ-005
title: registry.ohmygh.com 私仓直连与离线回落
status: draft
priority: could
trace: null
---

> 总台 OCI 集成边界裁定登记(2026-09-20,插队轻量单,零改动回执合法);归 roadmap 裁量。

# registry 私仓直连与离线回落

## Scenario

端上无 omc 二进制时,dctl 以原生 Rust HTTP 客户端直连 registry.ohmygh.com(registry:2,v2 协议,basic 凭据从本地密档读),拉取常用数据库镜像;上游不可达时按回落序工作。

## Criteria

- [ ] registry v2 API 客户端:/v2/_catalog、/v2/<name>/manifests、blobs;basic 凭据本地密档,零入仓零 argv
- [ ] 离线回落序:Docker Hub 上游直连到 registry.ohmygh.com 到本地缓存;常用镜像 latest 锚预置清单向总台提请
- [ ] docker-load tar 通道(docker save/load 格式)作端间转移兜底,语义复用不耦合代码;层遍历规范文档按需向总台 REQ 提请
- [ ] 与现有 Docker 引擎(install/start)的接入面与凭据安全边界另立 ADR 后动工
