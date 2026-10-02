---
id: REQ-0014
title: 项目作用域状态目录迁入 ~/.dctl 应用数据目录(不随 cwd 漂移)
status: implemented
priority: must
trace: 总台派单 2026-10-02(dctl 状态目录落仓问题);ADR-0012;实现批(桶寻址 + 迁移 + init 收敛 + 文档同步);实机冒烟:净 cwd 跑 server list 零落点、legacy .dctl/servers 迁入桶且旧壳清;评审轮 5519267 回执 F1/F2+G1-G3,修订批桶权威同名跳过、集成脚本桶化、Copy 无元数据数据目录拒绝、gitignore 内容校验、refusal 文案给 docker 路径
---

# 项目作用域状态目录迁入 ~/.dctl 应用数据目录

## Scenario

总台在册:ai_ccoe 与 ai-cloud 两仓各一枚 untracked `.dctl/`,为 2026-09-28 dctl 集成/双机部署轮在仓 cwd 下运行状态命令的残留(根因:持 `lock_metadata()` 的命令建 `.dctl/servers/.metadata.lock`,自忽略保障只在 start/init 路径)。用户裁定(2026-10-02):「dctl 应该有自己应用数据目录」,状态面不随运行 cwd 漂移;cwd 内 gitignore 兜底只算过渡,终态走固定应用数据目录。

## Criteria

- [x] 桶寻址:`~/.dctl/projects/<id>/servers/`,`id` = canonical cwd 的 sha256 前 16 hex;桶内 `project-path` 旁存明文;同 cwd 稳定、异 cwd 区分(单测)
- [x] 换址覆盖:servers_dir/servers_dir_join/ensure_*_data_dir/lock 全部指桶;cwd 零运行态落点(集成测试断言命令运行后 cwd 无 `.dctl/`)
- [x] 迁移:旧 `cwd/.dctl/servers/` 条目(除 `.metadata.lock`)入桶;同 fs rename;EXDEV 且有运行中实例或 Docker 不可达时 fail-closed 报可操作错误;旧壳按 ADR-0012 口径尽力清(注入式单测)
- [x] `init` 收敛:只建三引擎脚手架;`.dctl/.gitignore` 机制退役;InitOutput 与帮助文本、README、AGENTS.md 环境节同步
- [x] 解析/帮助面:cli.rs 契约注释改写后过结构断言(不钉措辞)

## 非目标

- XDG `~/.local/state/dctl` 新全局根(与既定 `~/.dctl/` 分裂,ADR-0012 已裁)
- 旧桶回收/桶 GC(YAGNI;`project-path` 明文留人排查)
- 运行中实例的跨设备在线迁移(显式拒绝,stop 后重试)

## 已知边界

- 项目目录改名后旧桶滞留(桶键随 canonical cwd 变化)
- `~/.dctl/projects/` 内数据目录为 Docker bind 源,备份纪律同 home 目录

## 验证判据

- 双 clippy 零警告、fmt 过、全量测试绿(计数随新增测试增加;fake-Docker 套件本机过,Docker socket 真连套件待 lan-linux 复验)
- WSL 本机实机冒烟(2026-10-02):净 cwd 跑 `server list` 后 cwd 无 `.dctl/`,桶落 `~/.dctl/projects/<id>/servers/`;预置 legacy `.dctl/servers`(json + data + 锁 + .gitignore)再跑,json 与 data 迁入桶、锁不搬、旧壳全清、`project-path` 明文落盘
