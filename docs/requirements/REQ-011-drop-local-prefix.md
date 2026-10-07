---
id: REQ-011
title: 去掉 local 命令前缀层,默认即本地操作
status: implemented
priority: must
trace: 39f6573(引擎命令免 local 前缀);现命令面真相 dctl --help
---

# 去掉 local 命令前缀层

## Scenario

用户令(2026-09-22):dctl 命令不需要多出一个 `local` 前缀,默认我们就是本地操作。`dctl local server start` 变 `dctl server start`,以此类推;顶层非 local 命令(skills、update、ledger)名面不变。

## Criteria

> trace 修订(2026-10-08,健康评审):本 REQ 以 **argv 垫片契约**交付,不是 clap 树扁平。`main.rs` 预处理把引擎命令提升到顶层,clap 树保留 `local` 层,两形解析等价、无弃用退出。全树重组是可选后续,不属本 REQ 验收;按「implemented = 垫片」读,勿据旧准则删兼容路径。

- [x] 前缀两形等价:`dctl server start` 与 `dctl local server start` 解析同一命令(server、postgres、falkordb、registry、install、init、client 经 argv 垫片提升;clap 树保留 `local` 层)
- [x] `--json` 全局面保留;CONTEXT FOR AGENTS 块随命令保留(无前缀调用时 usage 行渲染前缀规范形)
- [x] README 全部示例、AGENTS 命令引用、帮助文本以无前缀形态为主口径
- [x] 兼容形态:`dctl local ...` 旧形态持续可用(垫片达成等价,无弃用提示设计)
- [x] 退出码、结构化错误信封、agent JSON 面不受层级变化影响
