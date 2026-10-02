# 状态目录入应用数据桶:ADR-0012 布局迁移批

- 日期: 2026-10-02
- 批面:总台派单(两仓 untracked `.dctl/`)+ 用户裁定「dctl 应该有自己应用数据目录」

## 根因

- 两仓残留各只含 `servers/.metadata.lock`:持 `lock_metadata()` 的状态命令(ps/list/stop)在 cwd 建 `.dctl/servers/` 落锁,而自忽略保障(`ensure_runtime_gitignore`)只挂 start/init 路径。状态命令在未 start 的仓跑一次即落裸目录。
- 处置取舍:cwd 内补 gitignore 只是治标(运行态仍落用户仓);用户裁定终态 = 状态面归应用数据目录。直接做终态,不做过渡层。

## 布局与迁移(ADR-0012 / REQ-0014)

- 桶:`~/.dctl/projects/<id>/servers/`,`<id>` = canonical cwd 的 sha256 前 16 hex,桶内 `project-path` 旁存明文。`servers_dir()` 单点换址,`~/.dctl/` 沿用既定全局根(不开 XDG 新根,避免双根分裂)。
- 迁移 fail-closed:持桶锁把旧 `cwd/.dctl/servers/` 迁入桶;锁文件无状态不搬;同 fs rename(bind mount 沿 inode,运行容器无感);EXDEV copy 仅在元数据经 Docker inspect 判死之后,判不活/不可达即拒绝并给「stop 后重试」。旧壳迁空即清(锁、`.gitignore`、`.dctl/`),有用户文件则保留。
- `init` 收敛为纯脚手架;`ensure_runtime_gitignore` 机制退役;`Error::StateMigration` 进 output 错误映射穷尽 match(parity)。

## 踩坑

- **python 全文替换的双前缀事故**:测试文件里 `.join(".dctl/servers/default-pg18/data/...")` 桶化时,断言处(需剥 `default-pg18` 前缀,因为 `fresh_instance_dir()` 已含)与 staging 处(不需剥)文本相同,一刀切替换把 staging 也剥了,数据落到 `servers/data/` 裸目录,回滚 disposable 快照误判新鲜把 pre-existing 数据删掉。教训:**同形异义的路径替换必须逐处确认语义,不能全文替换后只靠编译过**;而且这类错误编译不炸、只有跑测试才现形。
- **`?` 进 `-> Error` 回滚函数**:回滚路径上桶寻址失败不能 panic 也不能吞主错误,降级为诊断行 + 跳过数据清理(instance_dir Option 化)。
- 集成测试的 `bucket_servers` helper 与二进制同算法(sha256 前 16 hex of canonical cwd display),两侧漂移会静默错桶,helper 注释已钉。

## 验证

- 双 clippy 零警告、fmt 过、全量 333 测试绿;check-md 42 文件干净;classify 门禁 OK。
- WSL 实机冒烟:净 cwd 跑 `server list` 后 cwd 零 `.dctl/`、桶落 home;预置 legacy(json+data+锁+.gitignore)再跑,迁入桶、旧壳全清、`project-path` 落盘。
- 待办:lan-linux 上真 Docker socket 套件复验(scripts/test-postgres-integration.sh);EXDEV 拒绝分支的真机验证(本机 /tmp 与 home 同 fs,单测已盖)。

## 评审轮(dctl-codex-review,首轮回执 2F+3G)

- F1 同名键覆盖:桶已有同名条目而 legacy 重现(旧版二进制又跑过/手工拷回)时,原实现 rename 静默覆盖桶 json、目录同名则 ENOTEMPTY 半合并中止。修:桶权威,同名跳过 + stderr 留痕 + 旧壳自然保留,单测与实机钉住(exit 0、桶不覆盖、异名照迁)。
- F2 集成脚本漏桶化:scripts/test-postgres-integration.sh 16 处 .dctl/servers 未跟批,脚本加 servers() helper(sha256sum 前 16 hex,与二进制实跑桶 id 对拍 MATCH)。
- G1 Copy 判活旁路:数据目录无兄弟 json 时不被扫描,fork 风险;修:Copy 前该形态直接 StateMigration 拒绝。
- G2 cleanup 保守性:.gitignore 仅内容等于自身写入形态 `*\n` 才删;读目录错误留痕且视为不可清。
- G3 refusal 文案:锁死态下 dctl stop 不可用,文案改指 docker ps --filter label=created_by=dctl + docker stop。
- 另:clickhouse 测试过时注释(git clean -xdf clears .dctl/)改词。

## lan-linux2 复验轮(lan-linux 关机,同版 Docker 29.8.1)

- 全量测试(真 Docker socket,uid 1000 + groups 999):**338 passed / 0 failed**,与本机一致。环境坑:lan-linux2 的 ssh 配置用户是 ubuntu(非 ray)、socket gid 999(非 983);持久 target/cargo 卷若 root 先写需 chmod -R a+rwX 再让 uid 1000 增量;容器必装 procps/git(util-linux 按需)。
- 首跑抓到一枚测试环境假设:copy_migration_refuses_..._without_docker 在真 Docker 下反转(不存在的容器 404=可证明停止即放行,行为正确)。重构为注入式探针(ensure_no_live_containers_with),三分支(探针不可达、在跑、证明停止)环境无关钉死。
- EXDEV 跨设备真机验证(/repo=ext4、/tmp=overlay):stopped 状态 copy 迁移成功(json+数据入桶);running 容器精确拒绝(「instance 'r-pg18' is still running」+ docker stop 指引),legacy 原样保留。顺手修 cleanup 缺口:跨设备迁移后 .dctl/ 变空目录原逻辑不清,现空目录与只剩自有 .gitignore 的情形统一清;remove 容忍 NotFound(并发窗口)。
- 集成套件 12/15:two_concurrent_servers、stop_all_engine_scopes、non_tty_query 三败,**基线 6465a52 同环境同复现**(c2 撞 5432),定性为既有缺陷:resolve_port 用 socket 探测占口,而 Docker 纯 iptables NAT 发布口在宿主无 listener,探测失明;旧机 lan-linux 应为 userland-proxy 模式故历史全绿。另因:Docker 端口探测应读 docker ps/inspect 的 published ports。与本批无关,记 REQ 候选。
- shell 验证脚本坑:printf 单引号模板里的 \" 原样输出坏 JSON(用 jq -n 生成);mktemp -d 目录 700 属 root,uid 1000 场景必须 chmod。

## 端口选择感知发布口(REQ-0015,封版批)

- 根因:resolve_port 的 TcpListener 探测对 iptables-NAT 发布口失明(宿主无 listener),NAT 模式机自动选口在 create 阶段才炸。修:docker::published_host_ports(list all 容器取 public_port),三引擎 resolve_port 注入化(_with 薄壳),explicit 口本地已占走零 daemon 快径。
- 测试:三引擎注入式单测、fk browser 排除、fake docker published_ports 注入(start 断言 5432 被跳选 5433);「无效输入零 Docker 请求」语义由惰性快径保住(初版无条件拉 published 被该测试抓住)。
- 顺批:集成脚本 run_case 每 case 隔离 HOME(孤儿桶不再落真实 HOME);ADR-0012 refusal 措辞对齐 G3;REQ-007/009/010/011 四枚状态滞后回填(009 注明 FK/CH 腿未立的边界)。
