---
id: REQ-010
title: 镜像拉取默认走私仓并可指定自定义仓库
status: draft
priority: should
trace: null
---

# 镜像拉取默认走私仓并可指定自定义仓库

## Scenario

用户令(2026-09-22):引擎镜像拉取默认走 registry.ohmygh.com(私仓优先,不再 Hub 优先),且能按次指定自定义官方仓/镜像源(如 quay.io、gcr.io、daocloud 等镜像)。

## Criteria

- [ ] 引擎 install/start 与 `registry pull` 的默认拉取源改为私仓优先,回落序变为:私仓、Docker Hub、本地缓存
- [ ] `registry pull` 与 `install` 支持 `--registry <url>` 按次显式指定拉取源(走私仓客户端协议,不走守护进程 Hub 腿)
- [ ] `DCTL_REGISTRY_URL` 旋钮语义不变(端点覆盖,改默认私仓地址);`--registry` 优先级高于环境旋钮
- [ ] 回落链各步 stderr 通报语义保持;私仓失败的回落原因可见
- [ ] 测试面:解析测试(--registry 旗标)、链序单测、stub 集成测试更新;lan-linux 真机复验(私仓优先断 Hub 不可达时链路正常)
- [ ] ADR-0010 记链序反转裁定,引用户令;ADR-0008 决策 1 加追注指向 ADR-0010
- [ ] 同步 README 与 AGENTS 的链序表述(「Docker Hub 优先」改为「私仓优先」口径)
