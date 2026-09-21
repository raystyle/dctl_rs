# dctl_rs

dctl(DataBase Control):ClickHouse/Postgres/FalkorDB 三引擎统一 Docker 容器生命周期管理 CLI(client 内置集成),是 ClickHouse/clickhousectl 的剪枝自维护 fork。公开契约以 `///` 契约注释与类型签名为准;命令面真相是 `dctl --help`。

`CLAUDE.md` 是一行 `@AGENTS.md` 桥接;只编辑 `AGENTS.md`,不另写第二份。

## Commands

- `cargo fmt --all`:提交前必跑(fmt.yml 门禁同款)
- `cargo clippy -p databasectl --all-targets -- -D warnings`
- `cargo test -p databasectl`
- `python3 scripts/tests/test_classify_install_integration.py`:改安装分类器或其路径映射后必跑

## Must

- 不可逆技术选择先立 `docs/adr/` 的 ADR 再动手;新需求先立 `docs/requirements/` 的 REQ 再写码
- 改命令面同步更新 clap 定义内帮助文本与 README,并补 `try_parse_from` 解析测试
- 推 main 前过 herdr 评审闸门:codex 回执轮次到 CONFIRM 才放行(流程见 `docs/guides/review-gate.md`)
- 每次交付跑双 clippy 配置与全量测试;依赖真实 Docker 的测试在 lan-linux 容器复验(配方见环境节)

## Must not

- 不写措辞钉死测试(`help.contains(...)`、`include_str!` README、整屏相等);只测结构(解析结果、默认值、隐藏旗标隐藏)
- 不把逻辑堆进 main.rs:单出口不变量,新命令处理器进 `src/local/` 专属模块(配方见 `docs/guides/adding-a-command.md`)
- 不改 builds.clickhouse.com 与 packages.clickhouse.com 产品下载 URL(那是 ClickHouse 产品的下载源,非本项目身份)
- 不手改生成物,不另写第二份命令面真相文档

## Read first

- 命令面:`dctl --help`;用户行为:README.md
- 决策与 why:`docs/adr/`(改对应决策时才读);需求:`docs/requirements/`
- 协作流程:`docs/guides/`;过程与踩坑:`docs/diary/` 与 `docs/research/`
- 源码检索:先 `crates/databasectl/src/local/cli.rs`(命令定义)到 `crates/databasectl/src/local/mod.rs`(分发)到专属模块

## 环境

- 开发机 WSL(/mnt/wsl/repos/dctl_rs,无 Docker);Docker 依赖测试用 `ssh ray@lan-linux`(Docker 29.8.1,无 Rust,以 rust:1-slim 容器跑,完整配方与权限坑见 `docs/diary/2026-09-20-fork-bootstrap.md`)
- 状态目录:项目级 `.dctl/`,全局 `~/.dctl/`;三引擎实例键 `<name>-<engine><version>`,容器名 `dctl-<engine>-<name>-<version>`
- 上游:`upstream` remote 指 ClickHouse/clickhousectl,选择性 backport,纪律见 ADR-0001
- 文档路径统一正斜杠写法 `docs/adr/`
