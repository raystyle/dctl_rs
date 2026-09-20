# S002 ledger 家族标准对齐研究

> 本文件 = 兄弟仓(hst_rs、reader_rs、ark_rs)ledger 集成实现与 dctl 的对照,产出对齐矩阵与行动清单;数据源为各仓源码一手抽查(2026-09-20),服务端契据为 ohmycloud workers/ledger/src/index.ts 与 codex 评审实证。结论供家族标准裁定,已对齐项与偏离项各留痕。

## 家族共性(四仓一致,dctl 亦已达标)

- 签名道:v1 七行签名基(方法/路径/时间戳/nonce/幂等键/body-sha256)、五头、Ed25519 base64url、kid = sha256(紧凑字母序 JWK) [实证: hst ledger.rs:111-129、reader ledger.rs:136-138、ark ledger.rs:107-116、dctl sign.rs]
- 事件体嵌套 payload:{type, body?, payload:{...}};close = result(payload:{digest}) 先行 + status(payload:{to:"done"}) 收尾 [实证: hst ledger.rs:423-449、reader ledger.rs:321-336、ark ledger.rs:338-364]
- note 放顶层 body(服务端只读 body) [实证: 三仓同形]
- digest 校验 sha256:<64 小写 hex>,与服务端 DIGEST_RE 同源
- 读面免签、写面签名、401/409/429 归因文案
- 命令帮助注明真源 ledger.ohmygh.com

## 对齐矩阵(dctl 偏离项)

| 维度 | 家族(三仓) | dctl 现状 | 裁定 |
| --- | --- | --- | --- |
| 命令挂载 | 顶层 Issue/Artifact(/Ledger)平级 [实证: hst main.rs:139-149、reader lib.rs:231-242、ark main.rs:239-245] | `dctl ledger` 组下挂 | 保持:dctl 命名空间更稠密(local/skills/update/ledger),组内聚合理;语义无差 |
| close 链幂等键 | hst 确定性 sha256("hst-issue-{n}-{type}-{digest}") [实证: hst ledger.rs:431];reader/ark 随机 [实证: reader ledger.rs:154、ark ledger.rs:125] | 随机 uuid v4 | **建议对齐 hst 确定性键**:半链重跑(currently 明示「会追加重复 result」)变为幂等回放,根治 G4;codex 亦持此议 |
| 幂等冲突(409) | reader 自动换键重试一次 [实证: reader ledger.rs:233] | 报错 + rerun hint | 可选:确定性键落地后 409 面收缩,暂保持 |
| 私钥形态 | base64url seed 经 env;密档路径各异 | PEM(PKCS#8);env 收内容或路径;密档 ~/.dctl/ledger/ | 保持:dctl 体系(PEM 与 openssl 生态互通);家族 keygen 命令(hst Ledger 组)对应 dctl `ledger key` 只读分发面,生成靠 openssl 一行,已写进指引文案 |
| 读面字段渲染 | 各仓自选 | 表格列 + 透传 JSON | 已达标(经 F2 修复) |
| 翻页 | limit+before+has_more+count 语义 | 同 + more=1 | 已达标(经 F3 修复);more=1 为 dctl 补齐的服务端要求,家族或同 |

## dctl 独有防御(正向超集,家族可反向吸收)

- 写面路径含 query 本地拒绝(签名基只盖 pathname)
- base_url 尾斜杠 trim
- 私钥 0600 警告
- 事件体/字段/翻页三处的服务端真实形状 wiremock 断言(接受侧语义)

## 行动清单

1. **确定性 close 链幂等键**(对齐 hst):idem = sha256("{REPO_ID}-issue-{n}-{type}-{digest}"),status 键锚 digest;重跑全链幂等回放。列为下一小单,经评审闸门后落。
2. 家族标准文本建议沉淀到 ohmycloud 总台(REQ-063 附錄或独立标准档):共性节即上表「家族共性」,dctl 三项防御列为推荐项。
3. reader 的 409 换键重试、hst 的 keygen 命令形态,记 backlog 参考。

[实证: 2026-09-20 三仓源码行号抽查 + dctl 评审会话 codex 交叉印证]
