# REQ-0016:Postgres 服务端 mTLS 腿落地

- 日期: 2026-10-08(深夜批,接健康审修复批同日)
- 批面:用户裁定「REQ-009 服务端 mTLS 腿」;REQ-0016 立项(判据自 REQ-009 迁出),设计真源 ADR-0011
- 终态:main `397011a..dd8ec52` 四笔合入(14 文件 +613/-51);lan-linux2 实弹 11/11 + 全套 357/0;双评审三轮双 CONFIRM;CI 三道 dispatch

## 实施(四笔)

- `94bb219` 主体:ca.rs `issue_server_cert`(容器名 CN + 127.0.0.1/::1 SAN);docker.rs 证书面 Cmd(`-c ssl=on` 族)+ `build_postgres_tls_tar`(四文件,uid/gid 999,key 0600)+ tar 上传(body_full;绕开 bind-mount 的跨 uid 权限陷阱);hba 三行制(local trust 供镜像入口脚本、hostssl cert、host reject);ServerInfo.tls 三态(None=旧实例口令面);`--auth cert|password`(默认 cert);client TlsMode 三态;dotenv 双形(PGSSL 四键 + verify-full / POSTGRES_PASSWORD)
- `c2add1c` 二轮修:kimi-F1 签发/上传并入回滚区;kimi-F2/grok-F1(交叉同击)Required 兜 tls_config 失败臂;grok-F2 信封变体归位(上传失败 PostgresUsage parity,daemon 明细走 stderr);grok-F3 CONTEXT/README 改证书面口径;grok-F4 ADR 追注时序;G1 剥离精确键化
- `b6f5504`/`dd8ec52` 三轮单句修:dotenv 尾块 export 句限定 PGSSL 四键;上传失败文案改述回滚后世界

## 实弹炮位(lan-linux2,绕 daemon loopback 限制)

服务器 TLS/hba 证毕用**容器内 psql 直轰**:证书认证 verify-full 返回 42(端到端)、明文 TCP 被 hba 拒(原文实证)、口令面回路、dotenv 两形。宿主 client 腿因该 daemon loopback 发布口不可达,Required 分支错误文案实证(单列记档,非码缺陷)。

## 评审链(三轮,交叉互补)

- 首轮:kimi 2F(回滚区外泄/Required 单臂闸)+ grok 4F(信封教义两处/CONTEXT 面实性/文档态时序);kimi-F2 与 grok-F1 交叉同击
- 二轮:各一条单句级残留(kimi:上传文案描述修复前世界;grok:export 句以偏概全)
- 三轮:双 CONFIRM
- 亮点:grok 追查到信封对 StartupRollback 按 primary 递归分类与上传失败 PostgresUsage parity 的链路自洽;kimi 定位上传块的回滚覆盖区缺口

## 踩坑

- hst secretguard 钩子拦 PEM 字面量:ca.rs 单测改无横线断言(`contains("CERTIFICATE")`)
- 假 Docker 夹具需教 PUT /containers/*/archive(证书面上传请求),否则 readiness 全套 SIGABRT(析构 panic 连锁)
- kimi 评审格 stall 假阴性已成常态:5 秒活动窗报 stalled,实况多为已吃单(context 爬升为真信号),补踢 agent send-keys enter 即活;本批两次
- grok 三轮快核回执文件名迭代(req0016-fixreview{,2,3}-grok.md),收件按 mtime 取最新

## 余账

- FK/CH mTLS 腿(ADR-0011 后续,REQ-009 draft 撑着)
- kimi-G:upload-500 断言 DELETE 的回归测试、Required tls_config 臂覆盖、ca 单测 fake-home 基建、证书面 start 输出的 Password 行标注、list 出 tls 字段(与信封 code 面同归契约统一裁定账)
