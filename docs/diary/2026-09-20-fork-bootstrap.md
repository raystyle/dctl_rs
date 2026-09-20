# 2026-09-20 fork bootstrap

> 本文件 = fork 当日的裁定、踩坑与门禁实录;不可逆决策已升格 ADR-0001 至 0004,本篇留过程。

## 当日裁定

- fork ClickHouse/clickhousectl,剪除 cloud,定位 DataBase Control(ADR-0001)
- 命名:仓库 dctl_rs、crate databasectl、二进制 dctl、状态目录 .dctl(ADR-0002)
- telemetry 默认编译排除、端点置空(ADR-0003);分发仅 GitHub Releases(ADR-0004)
- 协作模型:Claude 主 PR、codex 经 herdr 评审(F/G/CONFIRM)、Kimi 主测试(docs/guides/review-gate.md)

## 交付清单

fork-bootstrap 分支五提交:

1. `4c40ed6` prune:删 cloud 栈 157 文件,减 183484 行
2. `b3db092` rename:身份清扫 70 文件
3. `e5585b2` infra:发布/自更新/install 改指自己 GitHub Releases
4. `2ab268d` docs:新 README 与 AGENTS
5. `0ba6489` fix:telemetry 集成测试去 cloud 夹具

## 踩坑实录

- **测试输出尾部漏检**:修完 telemetry 单元测试后只看 cargo test 输出尾部,漏掉 telemetry_test.rs 集成套件 16 个 cloud 夹具失败;lan-linux 全量复跑才暴露。教训:全量验证必须 grep 全部 `test result` 行或数 FAILED 计数,不看尾部 [实证: 2026-09-20 本仓 commit 0ba6489 的由来]
- **grep 管道截断掩盖耦合**:`grep ... | head -20` 截断让 failure.rs 对 clickhouse_cloud_api 的引用漏检,编译才炸。教训:缝合面分析禁用 head 截断,或先数命中总数
- **rust:1-slim 容器四坑**(lan-linux 跑 Docker 依赖测试):缺 pgrep(procps)致 child_pids 测试失败;缺 git 致 init 测试失败;root 跑权限位测试假成功(0o500 挡不住 root);uid 1000 跑 Docker socket 拒绝(需补 socket 属组)

## lan-linux 测试配方

```bash
rsync -az --exclude target --exclude .git /mnt/wsl/repos/dctl_rs/ lan-linux:/tmp/dctl_ci/
ssh lan-linux 'docker run --rm -v /tmp/dctl_ci:/repo -v /var/run/docker.sock:/var/run/docker.sock -w /repo rust:1-slim bash -c "
  apt-get update -qq && apt-get install -y -qq procps git util-linux;
  mkdir -p /tmp/cargo /tmp/target /tmp/h && chmod 777 /tmp/cargo /tmp/target /tmp/h;
  setpriv --reuid=1000 --regid=1000 --groups=983 env CARGO_HOME=/tmp/cargo CARGO_TARGET_DIR=/tmp/target HOME=/tmp/h cargo test -p databasectl --features telemetry"'
```

要点:rsync 必须 `--exclude target`;远端 target 一旦被容器 root 写过,要用容器删(`docker run -v /tmp:/host rust:1-slim rm -rf /host/dctl_ci`);socket 属组 gid 以 `stat -c %g /var/run/docker.sock` 实查,当日为 983 [实证: 2026-09-20 按此配方全量 0 FAILED]。

## 门禁实录

- 本地:fmt 通过;clippy 双配置退出码 0;默认 feature 测试全绿
- lan-linux 容器(真 Docker socket):全量 0 FAILED,含 531 单测、全部 local 集成套件、42 telemetry 测试 [实证: 2026-09-20]
- 冒烟:dctl --version/--help/local init 正常,.dctl/ 状态目录就位
- 密钥扫描(scan.py)16 项 HIGH 全部裁定为假阳性:均为上游测试夹具(AWS 文档公开示例访问键、example.test 保留域凭据、HOSTILE 假凭据清单,测的恰是脱敏与不外泄),无真实凭据入库 [实证: 2026-09-20 逐条核对 network.rs 与 telemetry.rs 夹具源码]
- 骨架合规:check.py 十二项全 PASS [实证: 2026-09-20 PE-01 至 PE-12]
- 未做:codex 评审闸门待走;main 未推

## FalkorDB 引擎单

同日追加于 falkor-engine 分支:

- 立项:ADR-0005 + REQ-003(当日回填 implemented,trace 为 falkor readiness 套件)
- 交付:五提交(ADR/REQ、引擎主体、测试与文档、实弹修复);三引擎并存,pg/ch 零回归
- 实弹发现并修复两缺陷:docker exec 回退 client 未带认证;孤儿恢复把镜像全引用存成 version 致 resume 解析错位 [实证: 2026-09-20 lan-linux 真容器全生命周期 SMOKE OK]
- 测试写法教训:StartupExit 在 JSON 信封是 redacted 摘要,断言日志尾要用人类模式;串行锁要防毒化(lock 失败转 into_inner);假 Docker 的锁不变量只对慢操作(镜像 inspect/pull)生效,create/start 按设计持锁
- 环境特性记录:容器内跑 dctl(挂宿主 socket)时,TcpListener 端口探测在容器网络命名空间,看不到宿主端口占用;自动挑口在此环境可能撞宿主已绑端口,冒烟须显式指定端口

## ledger 命令族单

同日追加于 ledger-commands 分支(总台令 REQ-063 标准,REQ-004 落地):

- 交付:顶层 `dctl ledger` 命令域(issue 四命令 + artifact 四命令 + key 分发面),Ed25519 五头签名道,ADR-0006 立案
- 密钥:keypair 现场生成,私钥 0600 落 ~/.dctl/ledger/dctl_rs.pem 永不入仓不进 argv;公钥 JWK 常量内置 CLI
- 验证更正(请二评审沉淀):初版 wiremock 桩钉的是虚构响应形状(close 扁平事件体、id/seq 字段、无 more=1),套件绿不构成写面已验的证据;经对总台 worker 源码(index.ts)重钉真实形状(嵌套 payload、issue_n/artifact_id、more=1 触发 has_more)后重新全绿;读面实弹 GET 200 仍然成立 [实证: 2026-09-20]
- 踩坑:reqwest query 传单元组会触发 serde_urlencoded unsupported pair,须传键值对数组;ed25519-dalek v3 的 PEM 特性名是 pem(含 pkcs8),keypair 生成走 openssl 与生产密档同路径
- 待办:总台注册公钥 kid 后补写面实弹(issue new 201 + artifact publish 201)

## ledger 请二评审轮次

- 一轮(不放行):F1-F5 必修 + G1-G9。codex 直接读了 ohmycloud worker 源码、按真实语义写本地桩实跑、并拿一次性钥打真服务验证 401 文案:F1(close 事件体须嵌套 payload)、F2(响应字段 issue_n/artifact_id)、F3(游标 issue_n + more=1)、F4(桩形状虚构掩盖前三条)、F5(人类面丢 issue 号)
- 修复:事件体/字段映射/翻页/桩形状全按 worker 真实形状重钉;人类面表格带 Result/Dev/Prod/Cur 列;401 文案覆盖钥不配对;写面路径带 query 本地拒绝;base_url 去尾斜杠;私钥 0600 警告;README attest 旗标改 --kind;ADR-0006 补签名 pathname 语义与 DCTL_LEDGER_KEY 载体语义
- G3(b 案:从私钥推导 kid 并警告不匹配)与 G9(/events?since= 读面)记 backlog
- 教训:对有真源的服务做集成,桩形状必须从服务端源码抄,不能从契约摘要想象:三高一全因虚构形状而全绿

## 家族标准对齐轮herdr-flywheel 纯讨论轮

- 用户令「看其他项目如何实现的,形成标准对齐」;抽查 hst_rs/reader_rs/ark_rs 一手源码,成文 S002
- 核心发现:hst 的 close 链确定性幂等键(sha256 锚 issue 号+类型+digest)是家族对 G4 半链问题的既证解法,dctl 应对齐
- 保持项裁定:命令挂载(dctl 组式)、私钥形态(PEM)、表格渲染;三项 dctl 独有防御(query 拒绝、尾斜杠 trim、0600 警告)列为家族可反向吸收项

## 确定性幂等键对齐单

- PR #3 合入 main 后开工(deterministic-close-idem 分支):close 链两事件改锚定 (repo, issue, type, digest) 的确定性幂等键,半链重跑变幂等回放(S002 建议项落地)
- signed_post_with_idem 增固定键通道;wiremock 套件钉 64 位 hex 键形;close 帮助文案改 replay 语义
- 语义边界留痕(codex G3):确定性键锚 (repo, issue, type, digest),同 issue 同 digest 再关一次会回放不落新痕;当前服务端只增无改模型下合理,要留新痕须换 digest
- G1 采纳:wiremock 补端到端回放断言(同 close 跑两次,键对逐位相等);G2 采纳:409 文案分场景 + close CONTEXT 补 --note 复用纪律
