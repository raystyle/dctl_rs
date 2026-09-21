---
id: REQ-011
title: 去掉 local 命令前缀层,默认即本地操作
status: draft
priority: must
trace: null
---

# 去掉 local 命令前缀层

## Scenario

用户令(2026-09-22):dctl 命令不需要多出一个 `local` 前缀,默认我们就是本地操作。`dctl local server start` 变 `dctl server start`,以此类推;顶层非 local 命令(skills、update、ledger)名面不变。

## Criteria

- [ ] CLI 树去掉 `local` 层:server、postgres、falkordb、registry、install、init、client 提升到顶层
- [ ] `--json` 全局面保留;帮助面节序、CONTEXT FOR AGENTS 块随层级迁移
- [ ] README 全部示例、AGENTS 命令引用、帮助文本同步更新
- [ ] 全部集成测试 argv 去掉 "local" 首参
- [ ] 兼容窗口:`dctl local ...` 旧形态在过渡期内给一句弃用提示后退出 2(可选,待裁)
- [ ] 退出码、结构化错误信封、agent JSON 面不受层级变化影响
