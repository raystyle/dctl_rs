---
id: REQ-0015
title: 端口选择感知 Docker 已发布口(NAT 模式探测失明修复)
status: implemented
priority: must
trace: 状态桶批复验轮发现(2026-10-02,lan-linux2 集成套件三败,基线 6465a52 同复现,定性既有缺陷);实现批 8db8b66(docker::published_host_ports + 三引擎 resolve_port 注入化 + fake docker published_ports 注入);验证:本机与 lan-linux2 远端全量各 343 passed/0 failed,lan-linux2 集成 two_concurrent_servers 由败转绿(c1/c2 并发自动选口实测成立)
---

# 端口选择感知 Docker 已发布口

## Scenario

三引擎 `resolve_port`(clickhouse.rs / postgres.rs / falkordb.rs 各一份)以 `TcpListener::bind` 探测占口。Docker 纯 iptables NAT 模式(userland-proxy 关闭,如 lan-linux2)下,已发布口在宿主上**无 listener**,socket 探测误判空闲,start 的容器 create 阶段才报「port is already allocated」,自动选口在 NAT 模式机全废(实证:c1 占 5432 后 c2 start 即败,基线同复现;旧机 lan-linux 为 userland-proxy 模式故历史全绿)。

## Criteria

- [x] `docker::published_host_ports(docker)`:列出全部容器声明/发布的宿主口(含 stopped 声明,保守);Docker 不可达返回空集(探测退化为纯 socket,后续步骤自然报错)
- [x] 三引擎 resolve_port(及 ch `resolve_native_port_excluding`、fk `resolve_browser_port_excluding` 同链)以注入的占口集合参与判定:候选口在集合内即跳过(纯函数化,可单测)
- [x] 集成测试:fake Docker 的 `/containers/json` 注入已发布口,start 断言选中下一空闲口
- [x] lan-linux2 复验(NAT 模式):远端全量 343/0;集成 two_concurrent_servers 转绿(选口修复实证)。stop_all_engine_scopes 与 non_tty_query 仍败,归因该 daemon 环境限制:发布口对宿主 loopback 不可达(TCP 127.0.0.1:5432 实测 Connection refused,NAT 无 userland-proxy 且不覆盖 lo),CH readiness 与 pg client 均走宿主 loopback 故必败;修法在 daemon 配置侧(开 userland-proxy 或 hairpin),非 dctl 侧缺陷

## 非目标

- 按 (ip, port) 对精确面匹配(保守全拒:任何面已发布的口都跳过,代价仅是多跳一个候选)
- 修 Docker 环境本身(NAT 与否归部署侧)

## 已知边界

- stopped 容器声明的口也被跳过(过度保守,无 correctness 代价)
- 非 Docker 的外部占用仍靠 socket 探测(该面不盲)

## 验证判据

- 双 clippy 零警告、fmt 过、全量测试绿(本机与 lan-linux2 远端各 343,较上批 +5:三引擎单测三枚、fk browser 排除一枚、fake docker 注入集成一枚)
- lan-linux2(NAT 模式)集成:two_concurrent_servers 转绿;余两败归因 daemon loopback 环境限制(见 Criteria 注记),15/15 在该机不可达

## 实现注记

- explicit 口本地探测已占时零 daemon 往返(惰性:只有 socket 空闲的候选才拉 published 集),保住「无效输入零 Docker 请求」既有测试语义
- 保守全拒:任何面已发布的口都跳过(stopped 声明也算),不按 (ip, port) 精确匹配
