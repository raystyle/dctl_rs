# S004:FalkorDB mTLS 实测闸门(REQ-0017 先行判据)

- 日期:2026-10-08;环境:lan-linux2(Docker 29.8.1),镜像 falkordb/falkordb:v4.20.6(私仓链拉取)
- 结论:**全免密 mTLS 路线成立**,无需 ADR-0011 预留的「mTLS+口令半免密」降级

## 实测事实

| 探针 | 结果 |
| --- | --- |
| 基座版本 | Redis server v=8.6.3(falkordb 4.20.6 内嵌) |
| BUILD_TLS | 二进制含 `tls-cert-file` 旗标族(字符串计数 2) |
| CN 映射语义 | 二进制含 `tls-auth-clients` 与 `tls-auth-clients-user`;后者取值 **CN 或 off**(传用户名报错并给出合法值集) |
| 裸跑 TLS-only | `--port 0 --tls-port 6379` + 证书族 + `--tls-auth-clients-user CN`:起,明文连接 reset |
| 免密认证 | redis-cli `--tls --cert client.crt(CN=default) --key --cacert` PING 得 **PONG,零口令** |
| 模块查询 | 同通路 GRAPH.QUERY g "RETURN 1" 结果正常 |

## 关键坑(实施必守)

1. **旗标名**:`--tls-ca-cert-file`(8.6 新名)。旧名 `tls-ca-file` 不报「未知参数」而报
   `Unresolved Configuration(s) ... Module Configuration detected without loadmodule directive ... aborting`,
   极易误诊为参数顺序问题(实测顺序无关,裸跑与入口两路同错)。
2. **入口结构**:`/var/lib/falkordb/bin/run.sh` 为 entrypoint,redis 参数插槽 = `REDIS_ARGS` 与
   `FALKORDB_ARGS` 两个 env(后者在 `--loadmodule` 之后);镜像自带 `TLS=1` 开关是**半吊子**
   (gen-certs.sh 自签 + `--tls-auth-clients no`,仅加密零认证),不采用;dctl 走自签 CA 材料注入。
3. **证书材料**:服务器证书/密钥需容器内可读(run.sh 以 root 起 redis-server 后 setpriv 降权与否依镜像分支;实测 root 下 bind-mount 0600 可读,实施沿 REQ-0016 的 tar 注入道并按实测 uid 定属主)。
4. **TLS 参数落位**:两插槽均可(命令行解析不区分);dctl 实施把 TLS 族并入 REDIS_ARGS 管理
   (与 requirepass 同管理位),证书面丢 requirepass。

## 对 REQ-0017 的裁定输入

- FK 腿形 = 全免密 mTLS(tls-auth-clients-user CN + dctl CA 客户端证书,CN=DB 用户名)
- `--auth password` 回落 = 现状 REDIS_ARGS requirepass 道
- 客户端:redis-cli 实测通路如上;falkordb crate 的 TLS 客户端身份支持面待实施时核(rustls Connector 注入是否开放,不开放则经 redis-rs 低层或回落容器内 redis-cli exec 查询道(ADR-0009 交互道的既有形态))
