# dctl

**dctl**(DataBase Control)是本地数据库服务器管理 CLI:ClickHouse、Postgres、FalkorDB 图数据库三引擎统一走 Docker 容器生命周期,client 由 dctl 自身集成(ClickHouse 查询走 HTTP 接口,宿主无需装任何数据库 CLI)。它是 [ClickHouse 官方 clickhousectl](https://github.com/ClickHouse/clickhousectl) 的 fork(Apache-2.0),剪除了 Cloud 与二进制下载部分,保留本地引擎生命周期作为核心。

一条命令在项目目录里跑起数据库,无需手写配置:

```console
$ dctl local server start          # 需要时拉取 clickhouse-server 镜像,打印生成的密码
$ dctl local client -q 'SELECT 1'  # dctl 内置 HTTP 客户端直查
$ dctl local server stop
```

Postgres 与 FalkorDB 同一套容器生命周期:

```console
$ dctl local postgres start        # 需要时拉取 postgres:18,打印生成的密码
$ dctl local postgres client -q 'SELECT version();'
$ dctl local postgres stop
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
| `~/.dctl/` | 全局 | 命名部分配置 `configs/`、ledger 私钥 `ledger/` |

项目级命令只认当前目录下的 `.dctl/`,不向上搜索父目录;请在项目根目录运行。

环境变量:

- `DO_NOT_TRACK=1` 完全静默遥测,不改任何配置。
- `DCTL_TELEMETRY_URL` 把可选的遥测发送指向你自己的 collector。遥测默认编译排除(telemetry cargo feature);即使开启该 feature,未设置此变量也什么都不发。本 fork 永不向上游端点上报。

## 使用

顶层命令面:`dctl local`、`dctl skills`、`dctl update`。处处接受 `--json`,agent 自动获得;退出码:0 成功、1 错误、2 usage 错误、3 取消。

### 镜像预拉取

```console
$ dctl local install 26.8             # 或 26.8.9.10 / latest,预拉 ClickHouse 镜像
$ dctl local install postgres@18      # 或 postgres:17-alpine
$ dctl local install falkordb@4.20.6  # 或 falkordb:latest
```

### ClickHouse 服务器

```console
$ dctl local init                       # 脚手架 .dctl/、clickhouse/、postgres/、falkordb/ 目录
$ dctl local server start               # default 实例,clickhouse:26.8,双口被占自动选空闲口
$ dctl local server start dev --http-port 8333 --native-port 9333
$ dctl local server start --version 26.8.9   # 指定镜像 tag;latest 亦可
$ dctl local server list                # 三引擎并列(运行中 + 已停止)
$ dctl local server stop [NAME]         # 幂等;stop-all 停本项目所有引擎
$ dctl local server remove NAME         # 须先停止;删除容器与数据
$ dctl local client [-q 'SELECT 1']     # 内置 HTTP 客户端;-q/--queries-file 走 HTTP
$ dctl local client                     # 交互式,docker exec 进容器内 clickhouse-client
$ dctl local client --host H --port P -q 'SELECT 1'   # 直连任意 ClickHouse
$ dctl local server dotenv              # 写 CLICKHOUSE_* 连接变量
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
$ dctl local postgres start [NAME] [--user U --database D]
$ dctl local postgres client -q 'SELECT 1;'
$ dctl local postgres dotenv            # 写入 .env 连接变量
$ dctl local postgres stop [NAME]
```

停止保留容器以便恢复;remove 删除容器。生成的密码由 start 打印一次,之后经 `dotenv` 重读。

### FalkorDB 图数据库与 Docker

```console
$ dctl local install falkordb@4.20.6   # 或 falkordb@latest,预拉镜像
$ dctl local falkordb start            # 默认随机密码,6379 协议口 + 3000 Browser 口自动挑
$ dctl local falkordb client -q 'GRAPH.QUERY g "CREATE (:n {name: '\''root'\''})"'
$ dctl local falkordb client           # 交互式 redis-cli(宿主优先,回退 docker exec)
$ dctl local falkordb dotenv           # 写 FALKORDB_HOST/PORT/PASSWORD/BROWSER_URL
$ dctl local falkordb stop             # 保留容器与密码,可 resume
$ dctl local falkordb remove           # 须先停止;删除容器与数据
```

FalkorDB 是 Redis 模块图数据库(openCypher);`client -q` 直通 redis 命令,Cypher 参数记得加引号。Browser 可视化在 start 输出的地址。

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
$ dctl ledger artifact attest <id> --kind attest_dev      # 或 attest_prod/demote/supersede
$ dctl ledger artifact promote <id>
$ dctl ledger artifact list [--current] [--env dev|prod]
$ dctl ledger issue close 3 --digest sha256:<64hex>        # result 引 digest 后 status=done
```

真源 [ledger.ohmygh.com](https://ledger.ohmygh.com)。读面无需凭据;写面走 Ed25519 五头签名道,私钥从环境 `DCTL_LEDGER_KEY`(PEM 内容或路径)或 `~/.dctl/ledger/dctl_rs.pem` 读取,永不入仓、不进命令行。产物只登记内容哈希与元数据,不收二进制。

### 贡献者指南

开发纪律见 [AGENTS.md](AGENTS.md)(命令、硬约束、测试分类学、评审闸门)。本 fork 以 `upstream` remote 跟踪 ClickHouse/clickhousectl,选择性 backport 本地引擎改进。

## 许可证

Apache-2.0。dctl 派生自 ClickHouse clickhousectl;原始版权声明见 [LICENSE](LICENSE) 与上游仓库。
