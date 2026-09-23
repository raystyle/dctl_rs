# dctl

**dctl**(DataBase Control)是本地数据库服务器管理 CLI:ClickHouse、Postgres、FalkorDB 图数据库三引擎统一走 Docker 容器生命周期,宿主无需装数据库客户端(ClickHouse 走 dctl 内置 HTTP 客户端,Postgres 走内置 tokio-postgres,FalkorDB 走内置官方 falkordb 客户端;交互式 REPL 用容器内 psql/redis-cli)。它是 [ClickHouse 官方 clickhousectl](https://github.com/ClickHouse/clickhousectl) 的 fork(Apache-2.0),剪除了 Cloud 与二进制下载部分,保留本地引擎生命周期作为核心。

一条命令在项目目录里跑起数据库,无需手写配置:

```console
$ dctl server start          # 需要时拉取 clickhouse-server 镜像,打印生成的密码
$ dctl client -q 'SELECT 1'  # dctl 内置 HTTP 客户端直查
$ dctl server stop
```

Postgres 与 FalkorDB 同一套容器生命周期:

```console
$ dctl postgres start        # 需要时拉取 postgres:18,打印生成的密码
$ dctl postgres client -q 'SELECT version();'
$ dctl postgres stop
```

dctl 为 agent 而设计:检测到 coding agent 自动切换 JSON 输出;每个命令的 `--help` 尾部带 `CONTEXT FOR AGENTS` 块;错误以稳定的机器可读信封输出。

## 部署

安装预编译二进制(Linux musl 静态、macOS):

```console
$ curl -fsSL https://raw.githubusercontent.com/raystyle/dctl_rs/main/install.sh | sh
```

或用 cargo-binstall,直接读 release 元数据:

```console
$ cargo binstall databasectl
```

或从源码构建(需要 Rust stable,edition 2024):

```console
$ git clone https://github.com/raystyle/dctl_rs
$ cargo build --release -p databasectl
```

直接下载见 [GitHub Releases](https://github.com/raystyle/dctl_rs/releases),归档命名 `dctl-<target>-v<version>.tar.gz`。

`dctl update` 自更新到最新 GitHub release,`dctl update --check` 仅检查不安装。没有 crates.io、npm、PyPI 渠道;GitHub Releases 是唯一分发点。

环境要求:Linux 或 macOS;三个引擎都需要 Docker。

## 配置

dctl 的状态分两处存放:

| 路径 | 范围 | 内容 |
| --- | --- | --- |
| `<project>/.dctl/` | 每项目 | 服务器元数据 `servers/*.json`、服务器数据目录;由 `dctl local init` 写入 gitignore |
| `~/.dctl/` | 全局 | 命名部分配置 `configs/`、ledger 私钥 `ledger/`、私仓凭据与镜像缓存 `registry/`(`auth`、`cache/`) |

项目级命令只认当前目录下的 `.dctl/`,不向上搜索父目录;请在项目根目录运行。

遥测已整体退役(ADR-0003 / REQ-002):无采集、无上报、无相关环境变量。

## 使用

顶层命令面(`local` 前缀可省,两种形态等价;`clickhouse` 是 `server` 的别名;下文示例用无前缀形态):`dctl server`、`dctl postgres`、`dctl falkordb`、`dctl registry`、`dctl install`、`dctl init`、`dctl client`、`dctl skills`、`dctl update`、`dctl ledger`。处处接受 `--json`,agent 自动获得;退出码:0 成功、1 错误、2 usage 错误;3 不再由 dctl 自身产生(取消路径已随二进制引擎退役),仅作为容器内子进程的透传码出现。

### 镜像预拉取

```console
$ dctl install 26.8             # 或 26.8.9.10 / latest,预拉 ClickHouse 镜像
$ dctl install postgres@18      # 或 postgres:17-alpine
$ dctl install falkordb@4.20.6  # 或 falkordb:latest
```

### 私仓直连与离线回落

引擎 `install`/`start` 的镜像获取走透明回落链(ADR-0010):私仓 registry.ohmygh.com 优先(dctl 原生 v2 客户端拉取 OCI layout 并 `docker load`),失败转 Docker Hub(守护进程道),再失败用本地缓存 tar;私仓拉取成功后自动刷新缓存。`install` 与 `registry pull` 可用 `--registry <url>` 按次指定自定义 v2 源。显式走私仓道:

```console
$ dctl registry pull postgres:18   # 强制私仓拉取并刷新缓存;亦接受 name@sha256:<digest>
$ dctl registry catalog            # 列私仓 repos;读面匿名开放,有本地凭据则随行携带
```

私仓凭据从 `~/.dctl/registry/auth` 读取(`user:password` 一行,或 `Basic <token>`),或经 `DCTL_REGISTRY_AUTH` 环境变量注入(金库/CI 携带者,同 ledger 密钥约定);零入仓、零 argv、零日志。端点可由 `DCTL_REGISTRY_URL` 覆盖(运维/测试旋钮)。缓存即文件管理:删除 `~/.dctl/registry/cache/<slug>.tar` 即清理对应镜像。

### ClickHouse 服务器

```console
$ dctl init                       # 脚手架 .dctl/、clickhouse/、postgres/、falkordb/ 目录
$ dctl server start               # default 实例,clickhouse:26.8,双口被占自动选空闲口
$ dctl server start dev --http-port 8333 --native-port 9333
$ dctl server start nsm --bind 192.168.88.175  # 双口追加发布到该面(loopback 保留;0.0.0.0 全接口)
$ dctl server start --version 26.8.9   # 指定镜像 tag;latest 亦可
$ dctl server list                # 三引擎并列(运行中 + 已停止)
$ dctl server stop [NAME]         # 幂等;stop-all 停本项目所有引擎
$ dctl server remove NAME         # 须先停止;删除容器与数据
$ dctl client [-q 'SELECT 1']     # 内置 HTTP 客户端;-q/--queries-file 走 HTTP
$ dctl client                     # 交互式,docker exec 进容器内 clickhouse-client
$ dctl client --host H --port P -q 'SELECT 1'   # 直连任意 ClickHouse(要认证时加 --user/--password)
$ dctl client --queries-file seed/init.sql      # 一次一条语句(HTTP 接口限制;多语句文件请拆分或走交互)
$ dctl server dotenv              # 写 CLICKHOUSE_* 连接变量
```

start 时可用 `--config <name>` 把 `~/.dctl/configs/<name>` 部分配置以只读卷挂载进容器 `config.d/`。随机密码由 start 打印一次,`client`/`dotenv` 从容器环境重读。孤儿容器(元数据被移动)通过 Docker label 被重新发现。

### init 后的项目目录结构

`dctl local init` 在当前目录创建以下结构(幂等,已有则跳过):

```text
<project>/
├── .dctl/                  # 运行时状态(gitignore 自动写入,不入库)
│   ├── .gitignore          # 内容恒为 *,忽略整个 .dctl/
│   └── servers/            # 各服务器实例元数据与数据
│       ├── default-ch26.8.json   # ClickHouse 实例元数据(名称/容器/端口/版本)
│       ├── default-ch26.8/
│       │   └── data/       # ClickHouse 数据(bind mount 到容器)
│       ├── default-pg18.json   # Postgres 实例元数据
│       ├── default-pg18/
│       │   └── data/       # Postgres 数据(bind mount 到容器)
│       ├── default-fk4.20.6.json  # FalkorDB 实例元数据
│       └── default-fk4.20.6/
│           └── data/       # FalkorDB 数据(bind mount 到容器)
├── clickhouse/             # ClickHouse SQL 脚手架(可提交,含 .gitkeep)
│   ├── tables/
│   ├── materialized_views/
│   ├── queries/
│   └── seed/
├── postgres/               # Postgres SQL 脚手架(可提交,含 .gitkeep)
│   ├── tables/
│   ├── views/
│   ├── functions/
│   ├── queries/
│   └── seed/
└── falkordb/               # FalkorDB Cypher 脚手架(可提交,含 .gitkeep)
    ├── queries/
    └── seed/
```

**入库规则**:`clickhouse/`、`postgres/`、`falkordb/` 是你项目的 SQL/Cypher 脚手架(各含 `.gitkeep` 保证空目录入库),随代码提交;`.dctl/` 是运行时状态(服务器数据与元数据),`init` 自动写入 `.dctl/.gitignore`(内容为 `*`)确保整目录不入库。ledger 私钥在全局 `~/.dctl/ledger/dctl_rs.pem`(0600),不在项目目录内。

**多实例命名**:同一名字可有多版本实例(如 `default-ch26.8` 与 `default-ch26.9`、`default-pg18` 与 `default-pg17` 并存),元数据文件名 = `<name>-<engine><version>`;`stop`/`remove` 不带 `--version` 时,单实例直接选中,多实例报错要求指定。

### Postgres 与 Docker

```console
$ dctl postgres start [NAME] [--user U --database D]
$ dctl postgres client -q 'SELECT 1;'
$ dctl postgres dotenv            # 写入 .env 连接变量
$ dctl postgres stop [NAME]
```

停止保留容器以便恢复;remove 删除容器。生成的密码由 start 打印一次,之后经 `dotenv` 重读。

### FalkorDB 图数据库与 Docker

```console
$ dctl install falkordb@4.20.6   # 或 falkordb@latest,预拉镜像
$ dctl falkordb start            # 默认随机密码,6379 协议口 + 3000 Browser 口自动挑
$ dctl falkordb client -q 'CREATE (:n {name: "root"})'   # Cypher;--graph 选图(默认 g)
$ dctl falkordb client           # 交互式 redis-cli(容器内 docker exec)
$ dctl falkordb dotenv           # 写 FALKORDB_HOST/PORT/PASSWORD/BROWSER_URL
$ dctl falkordb stop             # 保留容器与密码,可 resume
$ dctl falkordb remove           # 须先停止;删除容器与数据
```

FalkorDB 是 Redis 模块图数据库(openCypher);`client -q` 接 Cypher 语句,结果按列对齐表格输出(2026-09-22 起,ADR-0009;此前透传 `GRAPH.QUERY` 等 redis 命令,迁移时把命令里的 Cypher 部分直接作为 `-q` 值、图名交给 `--graph`)。Browser 可视化在 start 输出的地址。直连 `--password` 会出现在进程命令行(ps 可见;ADR-0009 为直连面开的口子),敏感场景请用受管实例的存储凭据。

### agent 技能安装

```console
$ dctl skills --agent claude    # 把 ClickHouse agent 技能装进 coding agents
```

### ledger 公共账本

```console
$ dctl ledger key                                          # 打印内置公钥 JWK 与 kid(总台注册面)
$ dctl ledger issue new --title "修复 X" --kind bug --acceptance "判据"
$ dctl ledger issue list [--limit 100] [--before <id>]     # 家族翻页:has_more 饱和提示
$ dctl ledger issue show 3
$ dctl ledger artifact publish --name <名> --kind experience \
    --digest sha256:<64hex> [--version] [--git-range a..b] [--deps d1,d2]
$ dctl ledger artifact attest <id> --kind attest_dev      # 或 attest_prod/verification_failed
$ dctl ledger artifact list [--current] [--env dev|prod]
```

真源 [ledger.ohmygh.com](https://ledger.ohmygh.com),客户端实现为共享 [ledger-client](https://github.com/raystyle/ledger-rs) crate(v0.1.1,全舰队唯一签名道)。**本 CLI 只增不关不删**(总台权限收口):issue 关闭走 omc 工位 `omc ledger issue status <repo> <n> <to>`,删除走 `omc ledger issue delete`;产物的 promote/demote/supersede 同属 omc。读面无需凭据;写面私钥从环境 `DCTL_LEDGER_KEY`(PEM 内容、PEM 路径或 64-hex 种子)或 `~/.dctl/ledger/dctl_rs.pem` 读取,永不入仓、不进命令行。产物只登记内容哈希与元数据,不收二进制。

### 贡献者指南

开发纪律见 [AGENTS.md](AGENTS.md)(命令、硬约束、测试分类学、评审闸门)。本 fork 独立演进,不跟踪上游 remote(2026-09-21 裁定,见 ADR-0001 追注)。

## 许可证

Apache-2.0。dctl 派生自 ClickHouse clickhousectl;原始版权声明见 [LICENSE](LICENSE) 与上游仓库。
