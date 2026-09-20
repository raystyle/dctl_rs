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

## FalkorDB 引擎单(同日追加,falkor-engine 分支)

- 立项:ADR-0005 + REQ-003(当日回填 implemented,trace 为 falkor readiness 套件)
- 交付:五提交(ADR/REQ、引擎主体、测试与文档、实弹修复);三引擎并存,pg/ch 零回归
- 实弹发现并修复两缺陷:docker exec 回退 client 未带认证;孤儿恢复把镜像全引用存成 version 致 resume 解析错位 [实证: 2026-09-20 lan-linux 真容器全生命周期 SMOKE OK]
- 测试写法教训:StartupExit 在 JSON 信封是 redacted 摘要,断言日志尾要用人类模式;串行锁要防毒化(lock 失败转 into_inner);假 Docker 的锁不变量只对慢操作(镜像 inspect/pull)生效,create/start 按设计持锁
- 环境特性记录:容器内跑 dctl(挂宿主 socket)时,TcpListener 端口探测在容器网络命名空间,看不到宿主端口占用;自动挑口在此环境可能撞宿主已绑端口,冒烟须显式指定端口
