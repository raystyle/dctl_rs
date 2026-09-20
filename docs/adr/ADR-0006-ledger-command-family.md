---
id: ADR-0006
title: 集成 ledger 公共账本,顶层 ledger 命令域与本地密档签名道
status: accepted
date: 2026-09-20
deciders: [ray]
supersedes: []
superseded_by: null
tags: [ledger, integration, security]
---

# 集成 ledger 公共账本

## Context

总台令(REQ-063 标准)要求各仓 CLI 原生集成 ledger.ohmygh.com 公共账本(issue 加 artifact 双流 append-only,Ed25519 五头签名道),替代外置工具提交面。备选形态:外置脚本(违背 CLI 原生集成裁定)、动态推导 repo_id(从 git remote 推导,自作聪明且测试环境 remote 不定)、私钥经命令行传入(argv 会泄漏进 ps 与 shell 历史,总台令明禁)。

## Decision

顶层新增 `dctl ledger` 命令域(与 local/skills/update 平级),含 issue 与 artifact 两族命令加 `ledger key` 公钥分发面。repo_id 用常量 `github.com/raystyle/dctl_rs`,不从 git remote 动态推导;base URL 常量 ledger.ohmygh.com,留 DCTL_LEDGER_URL 环境覆盖供测试。私钥运行时从环境 DCTL_LEDGER_KEY(PEM 内容或路径)或默认本地密档 `~/.dctl/ledger/dctl_rs.pem`(0600)读取,永不入仓、永不进 argv;公钥 JWK(最小 {kty,crv,x})以常量内置 CLI,kid 为其规范化 JSON 的 sha256。签名基 v1 换行拼接(方法/路径/时间戳/nonce/幂等键/body sha256),Ed25519 签名 base64url;Idempotency-Key 与 Nonce 每次调用新生成 uuid v4。

## Consequences

- 好面:身份分发面闭环(CLI 自带验签材料,总台注册一次 kid 即通);密钥泄漏面最小化(argv 与仓均不触及);读面无签名无配额,立即可用
- 坏面:repo_id 写死,仓改名或迁移需改常量重发版;keypair 丢失则该 kid 身份作废,需总台重注册新公钥(无轮换协议前是一锤子买卖);事件体字段(result/status 的确切 schema)以契约摘要实现,写面实弹时可能需微调
- 注意:配额 per-key 50/UTC 日,CLI 不做本地计数,429 由服务端权威;幂等键语义(同键同内容回放、异内容 409)是服务端保证,CLI 每次新键
