# dctl

**dctl**(DataBase Control)是本地数据库服务器管理 CLI:以官方二进制管理 ClickHouse,以 Docker 容器管理 Postgres。它是 [ClickHouse 官方 clickhousectl](https://github.com/ClickHouse/clickhousectl) 的 fork(Apache-2.0),剪除了 Cloud 部分,保留本地与 Docker 引擎生命周期作为核心。

一条命令在项目目录里跑起数据库,无需手写配置:

```console
$ dctl local server start          # 首次运行自动安装 latest,拉起进程并等待就绪
$ dctl local client -q 'SELECT 1'  # exec 进入匹配的 clickhouse-client
$ dctl local server stop
```

Postgres 走 Docker 引擎,同一套生命周期:

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

环境要求:Linux 或 macOS;Postgres 引擎需要 Docker;下载 ClickHouse 二进制需要能访问 builds.clickhouse.com 与 packages.clickhouse.com(产品下载源,与上游一致)。

## 配置

dctl 的状态分两处存放:

| 路径 | 范围 | 内容 |
| --- | --- | --- |
| `<project>/.dctl/` | 每项目 | 服务器元数据 `servers/*.json`、服务器数据目录;由 `dctl local init` 写入 gitignore |
| `~/.dctl/` | 全局 | 已装版本 `versions/<v>/clickhouse`、默认版本标记、命名部分配置 `configs/` |
| `~/.local/bin/clickhouse` | 全局 | 指向默认 ClickHouse 二进制的符号链接,由 `dctl local use` 维护 |

项目级命令只认当前目录下的 `.dctl/`,不向上搜索父目录;请在项目根目录运行。

环境变量:

- `DO_NOT_TRACK=1` 完全静默遥测,不改任何配置。
- `DCTL_TELEMETRY_URL` 把可选的遥测发送指向你自己的 collector。遥测默认编译排除(telemetry cargo feature);即使开启该 feature,未设置此变量也什么都不发。本 fork 永不向上游端点上报。

## 使用

顶层命令面:`dctl local`、`dctl skills`、`dctl update`。处处接受 `--json`,agent 自动获得;退出码:0 成功、1 错误、2 usage 错误、3 取消。

### ClickHouse 版本管理

```console
$ dctl local install 25.12      # 精确构建、次版本系列,或 latest/stable/lts
$ dctl local list               # 已安装的精确版本
$ dctl local list --remote      # 可下载的次版本系列
$ dctl local use 25.12          # 设默认并符号链接 ~/.local/bin/clickhouse
$ dctl local which              # 查看默认版本
$ dctl local remove 25.12       # 带守卫:拒绝删除使用中或默认版本
```

### ClickHouse 服务器

```console
$ dctl local init                       # 脚手架 .dctl/、clickhouse/、postgres/ 目录
$ dctl local server start               # default 服务器,端口被占自动选空闲口
$ dctl local server start dev --http-port 8333
$ dctl local server status              # --global 可跨项目列出
$ dctl local server stop [NAME]         # 幂等;stop-all 停所有范围
$ dctl local server remove NAME         # 须先停止;删除数据
$ dctl local client [-q 'SELECT 1']     # exec 匹配的 clickhouse-client
```

start 时可用 `--config <name>` 把 `~/.dctl/configs/<name>` 部分配置叠加到托管服务器配置上。孤儿服务器(在项目里启动过但元数据被移动)通过进程 cwd 扫描被发现。

### Postgres 与 Docker

```console
$ dctl local postgres start [NAME] [--user U --database D]
$ dctl local postgres client -q 'SELECT 1;'
$ dctl local postgres dotenv            # 写入 .env 连接变量
$ dctl local postgres stop [NAME]
```

停止保留容器以便恢复;remove 删除容器。生成的密码由 start 打印一次,之后经 `dotenv` 重读。

### agent 技能安装

```console
$ dctl skills --agent claude    # 把 ClickHouse agent 技能装进 coding agents
```

### 贡献者指南

开发纪律见 [AGENTS.md](AGENTS.md)(命令、硬约束、测试分类学、评审闸门)。本 fork 以 `upstream` remote 跟踪 ClickHouse/clickhousectl,选择性 backport 本地引擎改进。

## 许可证

Apache-2.0。dctl 派生自 ClickHouse clickhousectl;原始版权声明见 [LICENSE](LICENSE) 与上游仓库。
