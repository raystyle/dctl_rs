# S001 clickhousectl 架构与工程体系研究

> 本文件 = fork 决策依据的上游研究档案(2026-09-20,三个并行 Explore 深读全仓);结论已被 ADR-0001 至 0004 采纳,全文留档备查。

## 定位与规模

ClickHouse 官方 CLI(clickhousectl,别名 chctl),2026-02-05 首提交,7 个多月 1253 commits,近 30 天 445 commits;主力一人(Alasdair Brown)重度 AI 辅助(codex merge 栈 PR) [实证: 2026-09-20 git log 统计]。3-crate workspace:clickhousectl(CLI)、clickhouse-cloud-api(OpenAPI 手写守护的云 API client,发 crates.io)、clickhouse-openapi-analyzer(私有漂移分析器)。src 约 10.9 万行,测试约 6.7 万行。

## 主 CLI 架构要点

- 纯 binary crate;local(ClickHouse 官方二进制直管进程 + Postgres 走 bollard Docker,零 shell-out)与 cloud 两子树完全正交:local 对 cloud 零代码引用 [实证: 缝合面 grep,全仓耦合仅 5 处]
- 双模输出:每个成功类型同时 impl Serialize + Display,`print_output(., json)` 单点分流;JSON 自动 = `--json` 或 is-ai-agent 检测,管道重定向仍人类可读
- 结构化错误信封:闭合 snake_case 码表 + parity(JSON message 等于人类文本)/redacted(外来文本策展替换)二分;退出码 0/1/2/3/4,ChildExit 透传
- 无显式状态机:状态 = 磁盘元数据(ServerInfo JSON)+ 运行时 liveness 的派生;MetadataLock flock + tempfile/fsync/rename 原子提交;孤儿恢复靠 lsof/ps 扫进程 cwd 反推归属
- 版本管理:staging + CommitLock 多锁无环,etag 缓存浮动构建,本地优先安装
- agent 友好面:每命令 after_help 内嵌 CONTEXT FOR AGENTS 块(硬上限 8 行),帮助树全量结构测试(不钉措辞)

## OpenAPI 防漂移管线已剪除留档

手写 client + 每日 analyzer 防烂:live spec 对 checked-in 快照与 Rust 代码三方比对,产出带 RFC 6901 指针的结构化 issue;弃用字段 cfg-gate 三明治(常量表 + 字段级 feature + analyzer 双向核对),默认构建下弃用字段不存在。方向感知 requiredness(response 全 Option 是策略,request 侧才严比)、豁免必须会过期(StaleExemption)。这套是云 API 专属,fork 无云故整体剪除 [实证: ADR-0001 剪枝后全量测试通过]。

## 测试与 CI 工程

- 分层:clap 解析测试贴定义、请求构造器单测、wiremock 子进程契约测试(cloud)、local 子进程一关注点一文件(21 个,假 Docker socket/假二进制/假 pgrep 隔离 PATH + env_clear)、纯逻辑内联;单元对集成约 89:11
- 真 Docker 只进 CI 两个 workflow;本地测试零 Docker 依赖
- 纪律:测试只断言结构,禁措辞钉死(help.contains、include_str! README、整屏相等)
- CI:action 全 SHA 固定;两个 fail-closed 路径分类器(文件增改必须登记,否则红);外部 PR 不注密钥

## 发布工程

一版四渠道(tag 驱动单 workflow 分 job):GitHub Releases 镜像到 builds.clickhouse.com CDN、crates.io(token)、npm 与 PyPI(OIDC trusted publishing);musl 静态产物 file/ldd 双验 + 8 发行版容器 smoke;binstall 元数据指 CDN;maturin 动态版本免手同步。自更新 24h 缓存 + 后台刷新 400ms 超时。

## 对本 fork 的采用清单

| 上游做法 | 采纳情况 |
| --- | --- |
| 双模输出单点分流 + parity/redacted 信封 | 原样保留 [实证: 531 单测含 output 契约测试] |
| local 子进程测试分类学与假替身 | 原样保留,Kimi 作业标准(docs/guides/review-gate.md) |
| 禁措辞钉死测试、action SHA 固定、fail-closed 分类器 | 原样保留(仅剩 install 分类器) |
| CONTEXT FOR AGENTS 帮助块 + 结构测试 | 原样保留 |
| AGENTS.md 活合同(现重构为五节合同) | 改造采纳 |
| cloud/OpenAPI/四渠道发布/telemetry 上游端点 | 剪除(ADR-0001/0003/0004) |

[经验: 上游证明 AI 主力开发能撑住千级 commit,靠的不是评审人海而是机器闸门制度化;本 fork 以三角色协作 + CI 第四评审者承接该结论]
