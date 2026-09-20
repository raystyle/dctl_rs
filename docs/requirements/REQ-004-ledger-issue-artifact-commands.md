---
id: REQ-004
title: 集成 ledger 标准的 issue 与 artifact 命令族
status: implemented
priority: should
trace: cargo test -p databasectl --test ledger_request_test
---

> 总台令登记(2026-09-20 插队,排队实施);契约全文见总台 ohmycloud 仓 REQ-063。

# 集成 ledger 命令族

## Scenario

开发者用 dctl 子命令族直接操作 https://ledger.ohmygh.com 公共账本:issue new/list/show/close 与 artifact publish/attest/promote/list,写入走 Ed25519 五头签名道,读面直连 GET。

## Criteria

- [x] 单测:签名基构造(v1 换行拼接)、幂等键语义(同键同内容回放、异内容 409)、kind/digest 校验
- [x] issue 面:POST /repos/github.com/raystyle/dctl_rs/issues 与 events、GET 家族翻页形(limit 100 + before 游标 + count 语义)
- [x] artifact 面:POST artifacts 与 attestations、GET current/env 过滤;digest 一律 sha256 正文哈希
- [x] 签名道:Idempotency-Key/X-Key-Id/X-Timestamp(±60s)/X-Nonce(10min)/X-Signature;私钥从环境或本地密档读,不进仓不进 argv;公钥 JWK 内置常量,kid 为规范化 JSON 的 sha256
- [x] help 注明真源 ledger.ohmygh.com;读面实弹 200,写面待总台注册公钥后补实弹
- [x] 回执:commit sha、门禁退出码、公钥 JWK 全文与 kid

## 验收补充

2026-09-20 评审闸门后:

- 三轮评审(F/G/CONFIRM 至无保留 CONFIRM)全闭合;F1-F6 与 G1-G13 逐条对照记录在评审会话与 diary
- backlog(不属完成态):G3-b(私钥推导 kid 并告警不匹配)、G9(/events?since= 读面)
