# 加一个 local 子命令

> 本文件 = 给 dctl 加命令的标准配方与帮助文本纪律;命令面真相在 `dctl --help` 与 clap 定义,本文件是操作指南。

## 三步配方

1. 在 `crates/databasectl/src/local/cli.rs` 的相关 enum 加 clap derive 变体(命令定义只住这里)
2. 在 `crates/databasectl/src/local/mod.rs` 的 `run()` 加 match 分发臂;处理器实现在 `crates/databasectl/src/local/` 下专属模块,不进 main.rs
3. 贴着命令定义补 `Cli::try_parse_from` 测试:解析值、默认值、隐藏旗标保持隐藏

## 输出与错误不变量

- 成功输出类型同时实现 `Serialize` 与 `Display`,统一走 `local::output::print_output(&out, json)`;JSON 模式 = `--json` 或检测到 coding agent(`json_output()`,main.rs)
- 运行期失败走 `local/output.rs` 的稳定信封:闭合 `LocalErrorCode` 词表,`parity`(JSON message 等于人类文本)或 `redacted`(外来子进程文本替换为策展摘要)
- 跨旗标约束 clap 表达不了的,进 `crates/databasectl/src/main.rs` 的 `validate_post_parse`,报成所属子命令的 usage error(exit 2)
- 退出码:0 成功、1 错误、2 clap usage、3 取消;`ChildExit(code)` 透传子进程码

## 帮助文本纪律

- 帮助屏只有:一行 `about`、clap 的 Usage/Arguments/Options/Commands、可选尾缀 `CONTEXT FOR AGENTS:` 块(硬上限 8 行内容,一行一事实)
- `about` 是祈使动词短语,无句号,同级平行;旗标帮助一行,含单位/格式,不复述 `[default: ...]`
- 共享旗标(`--json`)处处同文案;`help_order::JSON` 保证共享块排在尾部
- 帮助装不下的用户内容进 README,短示例或至多 3 行注记
