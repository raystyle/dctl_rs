---
id: REQ-006
title: 迁移到共享 ledger-rs crate 并统一权限收口
status: implemented
priority: must
trace: ledger-rs-migration 分支;PR 待开
---

> 总台统一裁(2026-09-20)两道令的合并登记:各仓 CLI 切共享 ledger-client 依赖 + 权限收口(只增不关不删)。

# ledger-rs 迁移与权限收口

## Scenario

dctl 移除自研 src/ledger/ 客户端代码,以 Cargo 依赖引入全舰队唯一实现;命令面按总台权限收口裁保留只增面,关闭与删除操作唯一道走 omc 工位经 herdr 委托。

## Criteria

- [x] Cargo.toml 引入 `ledger-client = { git = "https://github.com/raystyle/ledger-rs", tag = "v0.1.1" }`(总台追注:v0.1.0 有舰队级缺陷,URL 拼接缺斜杠致 GET 假空与 POST 路径分叉;v0.1.1 已修加 decode 硬化,hst 实弹报)
- [x] 删除 src/ledger/{sign,client}.rs(keys.rs 瘦身为密钥解析适配层:PEM/64-hex 解析为 KeyPair,签名道全在 crate) 的自研签名道与 HTTP 客户端(保留 cli.rs 命令定义与 output.rs 渲染,底层调 ledger-client)
- [x] 公钥 JWK 常量与 kid 约定不变(舰队一致:sha256hex 字母键序紧凑 JSON {crv,kty,x})
- [x] 权限收口:保留 issue new/list/show + artifact publish/attest(attest_dev/attest_prod/verification_failed)/list;移除 issue close、artifact promote/demote/supersede
- [x] 帮助与 README 同步:注明关闭与删除走 omc 工位(`omc ledger issue status <repo> <n> <to>` / `omc ledger issue delete`)
- [x] CHANGELOG 注明收口行
- [x] 测试:ledger-request 套件重写(离线可定面:key 解析/digest 前置校验/收口拒绝/identity 输出;HTTP 桩需 crate 开放 base 覆盖,已记追注)(桩指向 ledger-client 的请求形状);权限收口后的命令面 clap 解析测试
- [x] 随下版自然滚出,不强求独立封版

## 追注记录

- 2026-09-20 总台追注:依赖 tag 从 v0.1.0 改 **v0.1.1**(v0.1.0 舰队级缺陷:URL 拼接缺斜杠致 GET 假空与 POST 路径分叉;v0.1.1 已修 + decode 硬化,hst 实弹报)

## 实现追注(2026-09-21)

- ledger-rs 仓库已见 v0.1.2/v0.1.3(纯 fmt 整形,无 API 变化);按总台令钉 v0.1.1,后续 tag 升级零成本。
- HTTP 桩测试被 crate 的常量 BASE_URL 挡住(crate 的 with_base 仅 cfg(test));重写后的套件覆盖离线可定的全部面,HTTP 形状以 crate 自身单测为准。若需仓级端到端桩,建议 crate 开放 `pub fn with_base`(已反馈 ledger-rs 维护方)。
- dispatch 用 tokio::task::spawn_blocking 包裹:阻塞 client 内嵌 runtime 在 async 上下文 drop 会 panic(实测),不能直接同步调。
- 写面实弹探测(2026-09-21):真 CLI 发冒烟 issue,服务端回 401 "X-Key-Id 不在册、已吊销或不绑定本仓",此回应证明签名道与信封全链工作,唯 kid 注册待总台;注册后按舰队 write-face 轮复测即可。
- 写面复测收口(2026-09-21 下午):总台将 kid bc03b1ed…5149 直插 D1 pubkeys(status active,与 keys.rs 常量逐字一致);本侧 `dctl ledger issue new` 冒烟即 issue #1 注册成功、退出 0,401 消失。write-face 判据满足,关闭权在 omc 不变。
- **HTTP 投影形状的覆盖责任在 ledger-client 的单测 + 舰队 write-face 轮**,不在本仓:issue/artifact 列表的字段提取(rows/has_more/issue_n/artifact_id)若在服务端改名,漏网由 crate 侧测试与舰队实弹兜住,本仓测试只锁离线可定面。此为显式契约,不是缺口(评审轮 1 G5)。
