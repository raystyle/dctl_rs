# REQ-0017 FK mTLS 腿:fred 客户端道(2026-10-08)

## 终态

branch feat/req0017-fk-mtls-fred-client 合 main;lan-linux2 电池 10/10 + fred live 1/1;三轮评审(r1 修复批、r2/r3 双 CONFIRM);本机 367/0。

## 腿形

- 服务端(REQ-0016 机制平移):TLS-only 监听(--port 0 --tls-port 6379,tls-auth-clients-user CN,旗标名以 S004 为准),证书 tar 注入容器层 uid 0,readiness/交互 exec 走容器内 redis-cli TLS 旗标
- 客户端:证书面程序化查询 = fred v10(TlsConnector 直收 rustls ClientConfig,redis-rs 1.7 无此注入口,S004 补记),宿主直连 127.0.0.1:发布口;GRAPH.QUERY --compact 自定义命令 + 本仓解码器(镜像官方 crate 的 ParserTypeMarker 表与 GraphSchema 懒刷新,产出 FalkorValue)接既有渲染器,口令面(falkordb crate)输出同构;口令面与 direct 道不变
- dotenv 两形;ServerInfo.tls 恢复面(REDIS_ARGS 推 --tls-port);交互 REPL 仍容器内 exec

## 评审链(三轮)

- r1:kimi 2F+6G / grok 4F+4G;**F1 交叉同击**:fk_tls_cli_flags 把旗标+路径粘成单个 argv 元素,exec Cmd 无 shell 分词、redis-cli 整元素 strcmp,证书面探针必死。修前 lan-linux2 电池 8 例死 start(实弹复现),修后 10/10(闭环)。其余:kimi-F2 README FK 节/AGENTS.md 漂移;grok-F2 Vec32 Debug 镜像少枚举外层名;grok-F3 fred 库文进 parity 信封(三处拆 stderr+自撰);grok-F4 clap CONTEXT 口令世界残留
- r2:双 CONFIRM(e88c468)
- r3:双 CONFIRM(9ebb708)。实弹第二雷:cargo test 进程不走 main(),rustls 双 provider(ring 来自 fred/reqwest,aws-lc-rs 来自 reqwest 0.13 默认特性)致 ClientConfig::builder panic;ca.rs 两个 rustls 入口幂等装 ring。grok 深挖覆盖面:测试进程可达的全部 builder 点(含 reqwest 0.12/0.13 的 fallback、bollard ssl 门控)核尽

## 实弹(lan-linux2,发布口宿主 loopback 不可达)

- 电池 scripts/test-falkordb-integration.sh 十用例:TLS PING/GRAPH.QUERY 容器内直轰、明文拒、dotenv 两形、双 resume 保面、孤儿恢复 tls 面、双实例、版本隔离、运行中拒删:10/10
- fred live 腿(tls_cypher_live_round_trip,opt-in)1/1 过:三雷连环后闭环:provider panic(修),再到 SAN 拒绝(NotValidForNameContext,桥 IP 不在证书 SAN,产品按设计工作),最后是环境连败(OOM kill/出网 reset/挂载权限/git 依赖离线/sparse index 离线/glibc 反向不兼容)。终形 = **宿主 python3 转发器**(监听 127.0.0.1 抵容器桥 IP)+ 宿主编排(dctl 与测试二进制在本机编好 rsync 过去,rust:1-slim 容器以 uid 1000 + socket 组 999 跑,HOME 挂载持久):测试走完整生产路径(127.0.0.1 + SAN 校验真实发生 + fred 握手 + GRAPH.QUERY 真实 compact 回包解码渲染断言)
- 环境坑:deb.debian.org 与 static.crates.io 间歇 reset(同窗口 OOM kill 过 apt);离线 cargo 的四层依赖面(cache/index/git/config 对齐 registry URL hash 目录名)最终不如「本机编好带二进制」;WSL(2.39)编的二进制跑不动旧 glibc 宿主(2.35),要装进 rust:1-slim 容器跑;容器内 dctl 必须以 uid 1000 跑否则宿主编排读不了 metadata(root 属主残留同理要先清)

## 踩坑

- redis-cli 非交互整数裸打(raw 输出无 "(integer)" 装饰),管道断言 grep -qx 1;TTY 装饰形是假预期
- 测试进程 rustls provider:main() 的安装对 cargo test 无效;ca.rs 入口安装盖全部自有 builder(第三方 builder 点的可达性分析在 grok r3 回执)
- issue_server_cert 的 SAN 只签 loopback 是契约:桥 IP 动态不可签;测试借道须保 SAN 校验真实发生(socat 转发而非关校验)
- main.rs 旧注释「tokio-postgres-rustls 拉 aws-lc-rs」不准(真源是 reqwest 0.13 默认特性),行为无碍,下次触碰时顺手正
- reqwest/oci-client 在测试侧直拨 HTTPS 且不先触 ca.rs 会复现 provider panic(kimi r3 留白),归测试基建账

## 余账

- CH 腿(REQ-0017 后半):config.d openSSL 注入 + http_query reqwest identity
- 契约统一裁定账新入:证书面 start 输出 Password 行(PG+FK 同族);FK 证书面 Browser(3000 口)可用性(电池留记注探针);main.rs 注释正名;测试侧 HTTPS provider 预装
