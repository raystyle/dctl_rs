---
id: REQ-002
title: 物理删除 telemetry.rs 与 failure.rs 模块
status: draft
priority: should
trace: null
---

# 物理删除 telemetry 模块

## Scenario

ADR-0003 已把 telemetry 编译排除且端点置空;模块(约 3000 行,含 cloud 语义的失败分类词表)成为死重,留着会误导贡献者并拖慢编译。

## Criteria

- [ ] 删除 src/telemetry.rs、src/failure.rs、tests/telemetry_test.rs 与 Cargo `telemetry` feature、wiremock dev 依赖(若再无使用方)
- [ ] main.rs 单出口尾部简化,保持退出码契约 0/1/2/3 与 ChildExit 透传不变
- [ ] AGENTS.md 与 README 移除 telemetry 相关条目
- [ ] 全量测试与双 clippy 配置通过
