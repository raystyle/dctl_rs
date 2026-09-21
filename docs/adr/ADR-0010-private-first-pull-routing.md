---
id: ADR-0010
title: 镜像拉取链序反转为私仓优先,并支持按次自定义拉取源
status: accepted
date: 2026-09-22
deciders: [ray]
supersedes: []
superseded_by: null
tags: [registry, routing]
---

# 镜像拉取链序反转为私仓优先

## Context

需求: REQ-010。ADR-0008 决策 1 定的回落序是 Docker Hub 优先、私仓第二、缓存第三。用户令(2026-09-22)把私仓 registry.ohmygh.com 扶正为默认拉取源(Hub 降为回落),并要求能按次指定自定义官方仓或镜像源。

## Decision

1. **链序反转**:引擎 install/start 与显式拉取的默认链变为:私仓(oci-client 原生道)、Docker Hub(守护进程道)、本地缓存 tar 依次回落。私仓是常规路径而非故障路径。
2. **按次自定义源**:`registry pull` 与 `install` 加 `--registry <url>` 旗标,指定后该次拉取直接走该源的私仓客户端协议(oci-client),不掺 Hub 腿;链退化为:该源、本地缓存两级。
3. **旋钮层级**:`--registry`(按次)> `DCTL_REGISTRY_URL`(会话级端点覆盖)> 默认 `https://registry.ohmygh.com`。`--registry` 不写缓存旋钮语义,只改当次。
4. **通报语义**:各步回落仍 stderr 通报;私仓失败的原因(网络、凭据、缺镜像)在回落 Hub 前完整可见,排查不以 Hub 报错掩盖私仓报错。
5. **ADR-0008 决策 1 的回落序条款由本 ADR 取代**(其余条款不变);缓存刷新语义不变(成功的原生道拉取刷新缓存)。

## Consequences

- 私仓成为每次拉取的先导:私仓不可达时每笔拉取多一跳失败延迟(可观测、可回落,不是断路)。
- `--registry` 指向的源须讲 v2 协议(官方仓与主流镜像源都满足);Hub 的守护进程道不适用于自定义源。
- 集成测试的 stub 面不变(stub 本来就是 v2 面);链序断言按新序改写。
