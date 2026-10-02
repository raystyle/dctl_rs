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
