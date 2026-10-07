# 全量健康审与修复批:kimi+grok 双腿六轮闸门

- 日期: 2026-10-07 深夜至 2026-10-08
- 批面:用户令「kimi grok 进行项目 review」;v0.7.0 封版后的全项目健康审 + F 全修切片(用户定调)
- 终态:main `f5b2a02..1e410e0` 九笔合入,34 文件 +1123/-284;lan-linux2 Docker 面 352/0;CI 三道 dispatch 绿待收(test-install 为 pull_request-only 触发,其分类器面本地+远端双绿)

## 评审发起与发现面

- 本 tab 右侧建双评审格(dctl-kimi-review / dctl-grok-review,仓内 cwd,信任屏替答),双腿五件结构请求并行全量审(基线 f5b2a02,文件集枚举到行数)。
- **kimi 腿**:43 F(6 major / 37 minor)+ 36 G;首轮以 Agent Swarm 分组通读但工具连败零产物,产物从会话存储(`~/.kimi-code/sessions/` 的 wire.jsonl)盘上收割,主审终稿对 HEAD 逐条复核。
- **grok 腿**:7 F + 5 G,行为语义与漂移镜头(帮助文本/ADR-REQ 状态/信封覆盖)。
- 交叉命中 7 条(三方独立同击 main.rs 挂错子命令等),置信最高。

## 修复批九笔与六轮链

九笔:`ce73176`(端口/生命周期)、`d46e2d2`(凭据/吞错)、`bf3ba82`(进程/打印,SIGPIPE 冒烟 5/5)、`4c483a0`(契约文本/退出码/决策记录态)、`835e363`(SHA256SUMS 自更新对账+信封扩面)、`9896092`/`217ed96`(信封 parity 两轮收敛)、`d92f4df`(回滚死路补完+EXDEV 回归+两条漏项)、`1e410e0`(撤不成立的 docker rm 半句)。

六轮评审链,每轮都抓到上一轮的真问题(「全修不免二轮」实证):

1. 双腿修复批复核:kimi 3F(F-14 半修:就绪失败路径 save 先于探测,死元数据存活;EXDEV 回归:dotenv 裸名临时文件落 /tmp 跨设备;台账不实:F-09/F-26 漏修未记档);grok 2F(信扩后 redact 吃自撰补救文案;REQ 索引滞后)。
2. grok 快核抓二轮引入:Postgres/Download 整臂 parity 放进外来正文(tokio 驱动文本、bollard 原文)。
3. grok 快核抓三轮漏拆:hub_pull 的 bollard 文本改道 DockerError、resume 句挂 PostgresUsage 后 CONFIRM。
4. kimi 快核 CONFIRM d92f4df;grok 补轮否决 kimi 自己 G1 建议的「docker rm 保数据」半句(该点容器运行态 rm 必败;强删后元数据死指,下次 start 唯一出口 remove 反删数据)。
5. kimi 终裁采纳 grok 实证、自认 G1 失误;双 CONFIRM。

修复轮引入缺陷四例(过度 redact、过度 parity、半句不成立、chdir 竞态)全部被复核轮抓出,无一漏网到 main。

## 闸门与复验

- 每笔过全套:fmt、双 clippy 配置、cargo test(350 增至 352)、check-md、分类器测试;SIGPIPE 冒烟 5/5(关读端退出 -13 无 panic)。
- lan-linux2(rust:1-slim + 真 Docker socket,gid 999)全量 352/0,连此前记档的 loopback 顾虑项(stop_all_engine_scopes、non_tty_query)亦绿,该环境限制记录可放宽。
- 远端坑复发:/tmp/dctl_ci 下容器 root 遗留目录挡 rsync --delete,容器内 rm 清之(配方在案)。

## G 处置台账(记档不做,理由在回执)

首轮 23 条 + 修复轮 3 条:kimi-G3(skills_error/ledger_error 码)留待 code 面统一裁定;G4(prune 存量无标记永不剪)方向安全宁漏勿误;G5(pg client 连接失败 JSON 零细节)接受 redact 教义代价;其余为小 UX/防御性加固,逐条在 /tmp 回执与评审对话给理由。**SHA256SUMS 404 放行是过渡政策:首个带校验资产的 release 发出后须加版本下界或删该臂(grok G1,代码注释已记)。**

## 协作面踩坑(kimi 评审格)

- kimi 会话闲置两小时后弹缓存过期对话框吞派单;裁「新会话」(零上下文成本,首轮回执在盘上可自读)。
- 同格三次 agent_prompt_stalled(5 秒活动窗):两次为提交 Enter 未生效(agent 级 send-keys enter 补踢),一次真丢失(上下文零增长实证后重发)。纪律:stalled 不证未送达,先读屏核未达再重发。
- grep -c 零匹配返 1 会断 && 链(自家坑二次犯),门禁链改变量收集式。
