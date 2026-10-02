---
id: ADR-0012
title: 项目作用域状态迁入 dctl 自有应用数据目录,不再落运行 cwd
status: accepted
date: 2026-10-02
deciders: [ray]
supersedes: []
superseded_by: null
tags: [state-layout, storage, project-scope]
---

# 项目作用域状态迁入 dctl 自有应用数据目录

## Context

总台派单(2026-10-02)与用户裁定:dctl 在仓 cwd 下运行时落的项目级 `.dctl/`(servers 元数据、`.metadata.lock`、实例数据目录)污染被运行仓:ai_ccoe 与 ai-cloud 两仓各出现一枚 untracked `.dctl/`(2026-09-28 双机部署轮痕迹,内仅 `servers/.metadata.lock`)。用户裁定「dctl 应该有自己应用数据目录」:状态面不随运行 cwd 漂移。

根因:任何持 `lock_metadata()` 的命令(含 ps/list/stop 等状态命令)都会在 cwd 建 `.dctl/servers/` 并落锁文件;而自忽略保障(`ensure_runtime_gitignore`)只挂在 start/init 路径。状态命令在未 start 的仓运行即落裸目录。

## Decision

1. **桶布局**:项目作用域服务器状态全量迁至 `~/.dctl/projects/<id>/servers/`(`id` = canonical cwd 字符串的 sha256 前 16 hex;桶内 `project-path` 文件旁存 canonical 明文,排障可读)。元数据 json、`.metadata.lock`、实例数据目录全进桶。`~/.dctl/` 沿用 ADR-0002 以来的既定全局根,不开 XDG 新根(避免双全局根分裂)。
2. **换址单点**:`servers_dir()`/`servers_dir_join()`/`ensure_*_data_dir()` 系列整体改指桶内,项目隔离由桶键承担;`ServerInfo.cwd` 与 Docker label 的 canonical cwd 口径不变,recover/过滤行为不变。
3. **cwd 零落点**:cwd 不再产生任何运行态目录;`init` 只建 `clickhouse/`、`postgres/`、`falkordb/` 用户脚手架,`ensure_runtime_gitignore` 与 `.dctl/.gitignore` 机制整体退役(无落点则无兜底需求)。
4. **迁移(fail-closed)**:任意项目作用域命令首次在新版运行时,持桶锁把旧 `cwd/.dctl/servers/` 内容迁入桶,`.metadata.lock` 不搬(无状态);同文件系统逐条目 rename(bind mount 沿 inode,运行中容器无感);跨文件系统(EXDEV)copy+rm 仅在无运行中实例时执行,判据为元数据 container_id 经 Docker inspect 存活;有运行中实例或 Docker 不可达即中止迁移,错误文案给 dctl 之外的可执行指引(`docker ps --filter label=created_by=dctl` 定位并 `docker stop`,或恢复 Docker 可达;refusal 态下 dctl 自身的 stop 同样被迁移闸挡住,故指引不指向它)。桶为权威:legacy 与桶同名条目跳过不覆盖,留置给用户 reconcile。旧壳尽力清:`servers/` 迁空则删,`.dctl/` 只剩自身 `*` 形态 `.gitignore` 或已空则删,非空则保留由用户处置。

## Consequences

- 被运行仓的工作树不再出现 `.dctl/`;两仓既有残留(仅空锁)按「迁移无数据可搬、旧壳即清」路径自然消解。
- 同机同项目状态跨 cwd 重命名(目录改名/移动)后视为新项目(桶键随 canonical cwd 变),旧桶滞留(记为已知边界),`~/.dctl/projects/` 人可排查。
- 跨设备迁移在运行中实例上被显式拒绝(不静默丢 bind 写入),升级前需 stop;提示信息承担操作指引。
- AGENTS.md 环境节、cli 帮助文本、README 的 `.dctl/` 表述随之改写。
