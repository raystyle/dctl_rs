# 评审闸门与三角色协作

> 本文件 = dctl_rs 的交付协作流程(谁干什么、评审怎么轮次、何时能推 main);与 AGENTS.md 的分工是:AGENTS 只留一句硬约束,细节全在本文件。

## 三角色

- **Claude 主 PR**:按特性/issue 建分支,遵守 AGENTS.md 的 Must 与 Must not,保持每个提交独立绿(fmt、双 clippy 配置、测试)。
- **Codex 主 review**:在 herdr 窗格对交付批评审,评审通过前不推 main。发现逐条核实再修:高严重度断言也可能部分失真,且每个修复本身要过复核。
- **Kimi 主测试**:按测试分类学写测试(见下),禁止措辞钉死;覆盖缺口优先补结构断言。

## 回执三态与轮次

评审回执三态:F(必修,不改不合)、G(建议,采纳与否需答复理由)、CONFIRM(终审放行)。

轮次循环:评审请求到回执到修复并逐条对照到二轮快核,直至 CONFIRM;仅 CONFIRM 后才推 main。修复可能引入新缺陷,二轮快核不是走形式。

## 测试分类学

Kimi 的作业标准:

- **clap 解析**:`Cli::try_parse_from` 贴着命令定义写,断言旗标名、类型、默认值、可重复性
- **local 子进程**:`crates/databasectl/tests/` 一关注点一文件;假 Docker socket、假 clickhouse/psql/pgrep/lsof 进隔离 PATH;`env_clear()` 加临时 HOME
- **纯逻辑**:src 内联 `mod tests`,测版本解析、输出格式化、平台探测
- **帮助与 README 文本**:只做结构断言(节存在、行数上限、隐藏旗标不出现),不钉措辞
- **Docker 依赖**:`scripts/test-postgres-integration.sh`,在有 Docker socket 的环境跑

## CI 是第四个不睡觉的评审者

clippy 双配置 `-D warnings`、fmt、fail-closed 安装分类器、Docker 套件。人与评审 agent 的精力只花在机器判不了的面上:设计取舍、不变量是否被绕过。AI 写码速度远超任何评审速度,机器闸门才是真正兜底 [经验: 上游 clickhousectl 1253 commits 的实证]。
