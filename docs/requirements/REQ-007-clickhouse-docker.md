---
id: REQ-007
title: ClickHouse 引擎 Docker 化,三引擎统一容器管理
status: draft
priority: must
trace: null
---

> 用户裁定 2026-09-21:ClickHouse 也统一使用 Docker 维护,三库 client 在 dctl 内集成。ADR-0007 立案。

# ClickHouse Docker 化

## Scenario

开发者用 `dctl local server start` 管理 ClickHouse,与 Postgres/FalkorDB 走同一套 Docker 容器生命周期;`dctl local client -q 'SELECT 1'` 走 dctl 内置 HTTP 客户端(零宿主依赖),三引擎在 `server list` 统一展示。

## Criteria

- 契约注记(评审轮 1 F4):client 的 HTTP 查询面一次只执行一条语句(-q/--queries-file 各一条);多语句文件(如 init 生成的 seed)走交互模式或自行拆分。这是当前契约,非缺陷。


- [ ] `server start` 创建 ClickHouse 容器(双端口 8123+9000、数据 bind mount、配置 overlay ro 挂载、随机密码、ulimit 262144);`server stop/remove` 走容器生命周期
- [ ] 就绪探测:宿主侧 GET /ping 等 Ok.(三引擎中唯一可宿主探测)
- [ ] `local client -q 'SELECT 1'` 走 HTTP POST(body = SQL);`--queries-file` 同;交互走 docker exec TTY;直连(--host/--port)走 HTTP
- [ ] `local install <version>` 拉镜像;裸数字默认 ClickHouse;版本 tag 支持 4 段/minor/latest
- [ ] `server list` 三引擎统一(container_id 列);stop-all 覆盖三引擎;label 发现含 ClickHouse
- [ ] dotenv 写 CLICKHOUSE_HOST/PORT/USER/PASSWORD/DATABASE
- [ ] 退役:version_manager 全部、discovery 进程扫描、symlink 全局链接、flate2/tar 依赖、--foreground 旗标、local use/which/remove(ClickHouse 二进制部分)、List --remote
- [ ] FakeDocker readiness 套件 + client HTTP 合同测试 + start 参数测试
- [ ] 帮助块与 README 三引擎统一叙事;init 树更新
- [ ] 存量元数据兼容(旧 engine=clickhouse + pid 条目与新容器实例互不干扰)
