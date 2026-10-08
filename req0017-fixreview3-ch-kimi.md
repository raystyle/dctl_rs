# REQ-0017 CH 腿三轮快核(kimi 腿,增量 a3560f3,终审)

- 增量:f81825a..a3560f3 单笔;取证对本地 SHA 钉死内容
- 门禁本机复跑(a3560f3):370/0/1(与申报一致,+1 = database 白名单测试),fmt 净,双 clippy 0 error,check-md 53 净
- 结论:**CONFIRM**(附一条合并前置,见末节)

## 我 r2 两项的处置

- **r2-F1(注释病句)**:已清。server.rs:79-81 现为单句「Authentication face (ADR-0011, shared by all three engines): Some(true) certificate mTLS, Some(false) password, None = metadata predating…」,旧残段删除干净,病句与误标俱消。
- **r2-G1(--database 白名单)**:采纳且落点正确——validate_start_options 加 database 参数,规则 = 非空 + 仅字母数字下划线 + 不数字开头(保守臂,拒绝反引号形,可接受);口令面同样受益(env 道早失败);新单测五败一过在料。CH 双腿编译面与既有 bind 测试签名同步,无漏点。

## grok r2 3F 交叉复核

- **grok-F1(bootstrap 探针形)**:忠实且比我 r2 的 G 备注更彻底——不 attach 输出(--json 机器通道干净)、detach + inspect 轮询(150×20ms)、退出码为判、inspect 失败与 exit_code 缺席都判败(「绝不无声成功」注释在点)、信封自撰 + stderr 库文。tee 缺陷(grok-G1)随重构消解,我 r2-G2 同消。
- **grok-F2(PG resume 藏惰性口令)**:证书面 resume 输出 password 置空,与 fresh start 齐;口令面不变。该漏网我首轮只查了 fresh start,grok 此击成立。
- **grok-F3(CH resume 报存储库)**:resume database = prior.database 优先;恢复实例(database None)client 打明注记再落 default,不静默。✓

## 实弹凭证评估(我 r1/r2 放行条件 2)

- CH 电池 10/10:证书面全绿;password_face 用例 SKIP 记注的理由成立(口令面 readiness 走宿主 ping 是存量设计,该机 loopback 受限;CI runner 补),不阻。
- PG 电池 14/16:两败均为宿主连接类,与 REQ-0016 在册 loopback 限制同族同因;**F1 修复实证成立**(cert start/dotenv/resume/orphan 全绿,失败文案正确指向 trust chain 与发布口可达性)。
- 凭证粒度满足「机制点全绿 + 失败均在册环境类」的放行标准。

## 合并前置(唯一残留,流程级)

推送仍未发生:live `git ls-remote origin feat/req0017-ch-mtls` = **14fe18c**(两轮报备后依然;本轮 fetch 又遇一次 GnuTLS 断流,ls-remote 直连成功可证远端状态)。静态评审对 SHA 钉死的内容有效,但**合 main 前请以 ls-remote 见到 a3560f3 为准**;若届时 SHA 有变,回执作废重核。

**CONFIRM**:a3560f3 静态面全清 + 实弹凭证达标,可合 main(前置:推送落地)。
