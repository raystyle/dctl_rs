# S005 ClickHouse 腿 mTLS 标准配置检索

日期: 2026-10-08. 范围: REQ-0017 ClickHouse 腿,只研究,不改产品代码. 未重跑 fmt / clippy / test / check-md 全仓门禁,未在 lan-linux2 复测. 对照版本是用户实测镜像 ClickHouse 26.8.9.10;Git tag `v26.8.9.10` 与 `v26.8.9.10-stable` 的 entrypoint 路径返回 404,实存 tag 是 `v26.8.9.10-lts`. 置信级用四档:官方文档实证,源码实证(读到的就是该 tag 或本机已解析的 reqwest 0.13.3),issue 口径,推断.

已吸收、不再当待核项的实测: `config.d` 覆盖 `openSSL/server` 加 `https_port` / `tcp_port_secure` 能 merge;主 `users.xml` 的 `default` 已有 `password` 时,再叠加 `ssl_certificates` 报 `Cannot specify multiple authentication methods`;`<default replace="replace">` 整节替换后该错消失;标签 `<cn>` 报 `Unknown certificate pattern type: cn`.

## 1. users.xml 证书用户的 XML 形

结论: pattern 标签只有两种,可重复. 子元素名是 `common_name` 与 `subject_alt_name`,不是 `cn`. 同一用户写多个 pattern 就是多个同级子元素. `subject_alt_name` 的文本必须自带类型前缀,官方示例是 `DNS:host.domain.com` 与 `URI:spiffe://foo.com/*/bar`. 不带前缀的 SAN 模式对不上证书里带前缀的 SAN,等于不匹配.

匹配分桶,不交叉: `common_name` 只跟证书 CN 比,`subject_alt_name` 只跟证书 SAN 比. 先做整串相等;否则若模式里有一个 `*`,这个星号只匹配一段. CN 或 `DNS:` 的一段是不含 `.` 也不含 `/` 的非空标签; `URI:` 的一段只禁止 `/`,允许空段. 没有第二种星号语义.

dctl 现有客户端证书是 CN 等于用户名,没有 SAN(`ca.rs` 的 `client_cert`). 因此 CH 腿的用户节只写一条 `<common_name>用户名</common_name>`,不要改成 DNS SAN,也不要发明 `<cn>`.

置信: 官方文档实证(标签名与示例)加源码实证(26.8.9.10-lts 解析器与匹配函数). 用户实测的 `Unknown certificate pattern type: cn` 与解析器抛错一致.

## 2. verificationMode 与无证书客户端

结论: 值域是 `none` / `relaxed` / `once` / `strict`.

- `none`: 加密,不核验证书.
- `relaxed`: 出示了证书就校验,不强制出示.
- `once`: 服务端只在初次握手校验客户端证书,同样不强制出示;客户端侧等同 relaxed.
- `strict`: 必须出示且完整校验. 缺失,过期,或不是受信任 CA 签发,连接失败.

`strict` 就是「强制客户端证书」,失败点在握手层,没有 HTTP 状态码,不是 401. `relaxed` 与 `once` 下无证书握手能过,然后走普通认证.

两份官方文档对 dctl 这种自管官方镜像的取舍一致: 配置 TLS 指南把 `strict` 标成生产建议;证书用户指南写明安全的证书认证要用 `strict`,`relaxed` 只适合测试. ClickHouse Private 的文档建议用 `relaxed`、不建议 `strict`,那是另一产品(operator 交互),不要拿来改这条.

`verificationMode=strict` 只要求对端证书受信任,不要求证书 CN 等于 TCP 对端地址. 客户端是否核对服务端主机名是另一项(`openSSL.client`),不要和这条混写.

端口号是配置,不是协议常量. 官方示例常用 https 8443 与 `tcp_port_secure` 9440. 用户把 `https_port` 放在 8123、明文口挪到未发布端口,与文档不冲突.

置信: 官方文档实证. 未把 Poco 的 `SSL_VERIFY_*` 位映射写进结论.

## 3. X-ClickHouse-SSL-Certificate-Auth: on

结论: 头的值必须精确等于 `on`. HTTPS 加受信任客户端证书加该头,才走证书认证. 用户名来自 `X-ClickHouse-User`,不是来自证书 CN,也不是来自 query 参数 `user`. 头在、但 `X-ClickHouse-User` 为空时,用 `default_session_user`(未改配置时是 `default`),再用该用户的 pattern 去对证书. 证书 CN/SAN 都空,直接失败,原文是 `SSL certificate authentication requires nonempty certificate's Common Name or Subject Alternative Name`.

不带该头时,即使握手里出示了客户端证书,HTTP 仍走口令路径(头、Basic 或 query). 不是匿名成功,也不是证书认证. 证书只被记进 session,不参与这次登录. 用户若是纯证书用户,口令路径会认证失败.

与 `user` 参数: 该头一旦为 `on`,query 里出现 `user` 或 `password` 就在登录前被拒,理由是证书认证与 `authentication via parameters` 混用. 因此 `user=default` 加上该头会错,不是「多写一个无害参数」. 同时禁止非空的 `X-ClickHouse-Key`,也禁止 `Authorization` 头. 正确组合是该头加 `X-ClickHouse-User: 用户名`,URL 上不带 `user` / `password`.

这和原生协议相反. 原生协议自 23.3 起,客户端配置里带了证书就会强制证书认证,不再回退口令(issue 48974;官方证书用户指南也写明此时传给 clickhouse-client 的口令被忽略). HTTP 必须靠这个头做开关.

认证失败在公开 issue 里的形态是 HTTP 403 加 Code 516 `AUTHENTICATION_FAILED`,不要写成 401. 源码里的 401 只出现在 Basic / Negotiate 挑战那条路径.

置信: 源码实证(tag `v26.8.9.10-lts` 的 `authenticateUserByHTTP.cpp`)加官方文档实证(curl 示例)加 issue 口径(403 / 516,以及原生协议无回退).

## 4. 官方镜像 entrypoint 与 users.d

结论: 函数是镜像内 `docker/server/entrypoint.sh` 的 `manage_clickhouse_user`. 它只写这一个文件: `/etc/clickhouse-server/users.d/default-user.xml`. 不写 `no_password`.

环境默认: `CLICKHOUSE_USER` 缺省为 `default`,`CLICKHOUSE_PASSWORD` 缺省为空,`CLICKHOUSE_DB` 缺省为空,`CLICKHOUSE_SKIP_USER_SETUP` 缺省为 0,`CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT` 缺省为 0. `CLICKHOUSE_PASSWORD_FILE` 若指向已存在文件,会先读进 `CLICKHOUSE_PASSWORD`.

分支(bash 里 `&&` 优先于 `||`):

1. `CLICKHOUSE_SKIP_USER_SETUP=1`: 什么都不写. 留下镜像自带的 `default`(空的 `<password></password>` 仍是口令法,网络 `::/0`). 证书面不要用这个开关,否则空口令与 `ssl_certificates` 又变成双法,除非用户文件自己 `replace` 掉口令.
2. 用户名非空且不是 `default`,或口令非空,或 access management 不是 0: 写入 `default-user.xml`,用 `<default remove="remove">` 删掉 default,再按 `CLICKHOUSE_USER` 建用户,带 profile、quota、`::/0`、CDATA 口令、access_management.
3. 否则比较「原始 users.xml 的 users.default」与「合并后的 users.default」哈希. 已有 users.d 改过 default 则什么都不写.
4. 否则写入 `default-user.xml`,且不带 replace,只把 `default` 的 networks 收成 `::1` 与 `127.0.0.1`. 日志原文: neither CLICKHOUSE_USER nor CLICKHOUSE_PASSWORD is set, disabling network access.

users.d 在主 users.xml 之后合并,按完整路径字典序;标准目录下等价于文件名排序. `conf.d` 若同时存在,排在 `users.d` 之前. `replace` 属性存在即可(文档示例值是 `replace`,另一处示例值是 `1`): 只保留带该属性的节点的子节点,其余子节点丢弃. `remove` 是删掉该节点. 没有这两个属性则递归合并.

坑:

- `replace` 会丢掉未重写的 `password`、`networks`、`profile`、`quota`、`access_management`、`named_collection_control`. 证书文件必须把还要的项全部重写. 主文件里 `<password></password>` 也算一种认证法,所以不 replace 就和 `ssl_certificates` 互斥,与实测一致.
- 文件名 `dctl-...` 排在 `default-user.xml` 之前(`c` < `e`). 若容器层里已经留着 entrypoint 写的 `default-user.xml`(先前口令面或锁网那次启动),后合并的锁网会把 networks 盖回 localhost. 发布端口进来的客户端在容器里是网桥地址,不是 127.0.0.1,锁网后证书即使有效也会被拒. 文件名应排在 `default-user.xml` 之后,例如 `zz-dctl-user.xml`,并且 `replace` 后重写 `::/0`.
- 文件必须在 entrypoint 跑之前就在 `users.d`(bind-mount,或 create 与 start 之间拷入). 这样哈希比较会认为 default 已改,entrypoint 不再写 `default-user.xml`. 启动之后再拷,要等下一次启动才进合并.
- 省略 `<networks>` 不等于官方默认 `::/0`. 解析器只有键存在时才清空并重填允许地址;省略后的默认值本单没有单独读完. 实施时显式重写 networks,不要靠省略.

置信: 源码实证(tag `v26.8.9.10-lts` 的 entrypoint.sh 与 UsersConfigParser.cpp)加官方文档实证(配置合并、字典序、replace/remove).

## 5. clickhouse-client 的 secure 旗标

结论: `--cafile`、`--cert-file`、`--key-file` 都不存在. 本单读到的 master `programs/client/Client.cpp` 里,相关旗标是 `--secure` / `-s`(Use TLS connection)、`--no-secure`、`--tls-sni-override`、`--port`、`--host`、`--user` / `-u`(默认 `default`)、`--password`、`--config` / `-c`、`--accept-invalid-certificate`. 帮助示例是 `clickhouse-client --secure --host ... --password ...`. `--ssh-key-file` 是 SSH,不是 TLS 客户端证书.

客户端证书写在客户端配置的 `openSSL/client` 下: `certificateFile`、`privateKeyFile`、`caConfig`,并把 `loadDefaultCAFile` 设为 false. `--accept-invalid-certificate` 会把客户端校验降成接受无效证书,证书面不要用它代替 CA.

`verificationMode=strict` 时,容器内 REPL 必须出示客户端证书,否则握手失败. 姿势:

```text
clickhouse-client --secure --host 127.0.0.1 --port 9440 --user USER --config /path/to/client.xml
```

`client.xml` 的 `openSSL/client` 指向容器内的证书、私钥与 CA. 原生协议下,配置里一旦有客户端证书,口令被忽略(见第 3 项). dctl 服务端证书的 SAN 已含 127.0.0.1 与 ::1,连 127.0.0.1 时名字校验能过,不要靠跳过校验.

置信: 源码实证(master Client.cpp 的旗标表;26.8 文档与帮助示例同形,未再对 26.8.9.10 的 Client.cpp 逐行 diff)加官方文档实证(客户端 openSSL 配置与证书用户指南).

## 6. 口令面回落: 不设 PASSWORD,用 users.d replace

结论: 26.8.9.10 的 entrypoint 不会在无 env 时注入 `no_password`. 「无口令 env 就会抢先写成 no_password 并和证书冲突」这个坑不存在.

真正的冲突是这三件:

1. 镜像 `users.xml` 的 `default` 带空 `<password></password>`. 不 replace 就和 `ssl_certificates` 双法互斥. replace 掉口令后该错消失,与实测一致.
2. 无 USER/PASSWORD 且 users.d 还没改过 default 时,entrypoint 把网络锁到 localhost. 宿主机经发布端口访问会被拒. 见第 4 项的文件名与「启动前就挂上」.
3. `CLICKHOUSE_DB` 非空,或 `/docker-entrypoint-initdb.d` 非空时,entrypoint 会拉起一次只听 127.0.0.1 的临时 server,再用明文 `tcp_port`(不是 `tcp_port_secure`)执行 `clickhouse-client --host 127.0.0.1 --port "$NATIVE_PORT" -u USER --password PASSWORD` 做 `CREATE DATABASE`. 纯证书用户没有口令,这一步失败. 若把明文 `tcp_port` 整个删掉,`NATIVE_PORT` 为空,这条命令同样失败. init 仅在 `CLICKHOUSE_DB` 为空且 initdb.d 为空时跳过.

证书面的 env 形: 不设 `CLICKHOUSE_PASSWORD`,不设 `CLICKHOUSE_PASSWORD_FILE`,`CLICKHOUSE_USER` 保持未设或 `default`(设成别的名字会走进「删 default、建口令用户」). 不设 `CLICKHOUSE_DB`. 不用 `CLICKHOUSE_SKIP_USER_SETUP=1`. 数据库等 HTTPS 证书客户端就绪后自己 `CREATE DATABASE`.

扁平 XML 不能把口令和 `ssl_certificates` 放在同一用户上(解析器计数大于 1 即抛错,issue 86571 描述的就是这件事). 多法并存的官方 XML 形是 `<auth_methods>` 下列出多段,且 `no_password` 不能与其他法组合. 证书面要的是唯一法,不要用 `auth_methods`,也不要留 `no_password`.

置信: 源码实证(同一 tag 的 entrypoint 与 UsersConfigParser). issue 86571 与解析器抛错一致,本单未再查该 issue 的开关状态.

## 7. reqwest 0.13 客户端证书

结论: dctl 的 reqwest 是 `0.13`,features 含 `rustls`,不含 native-tls. 这条路径用 `Identity::from_pem`,入参是一块 PEM,里面同时有私钥和至少一张证书. 解析器把块里所有证书段和私钥段收齐,不要求调用两次,也没有「必须先证书后私钥」的第二次 API. 私钥格式是 PKCS#8、PKCS#1 RSA 或 SEC1 EC. `Identity::from_pkcs8_pem(cert, key)` 只在 native-tls feature 下存在,本依赖用不到.

根证书不要只靠已标为 Deprecated 的 `add_root_certificate`. 0.13.3 里该方法只是把证书推进 `root_certs`,且 `tls_certs_only` 仍为 false. rustls 分支在该标志为 false 时走 `rustls-platform-verifier`: 无额外根就用平台校验器,有额外根则 `new_with_extra_roots`(并进平台根). 私有 CA 的正确姿势是只信任这块 CA:

```text
Client::builder()
    .tls_certs_only([Certificate::from_pem(ca_pem)])
    .identity(Identity::from_pem(key_pem_concat_cert_pem))
```

再加两个头: `X-ClickHouse-SSL-Certificate-Auth: on` 与 `X-ClickHouse-User`. 不要加 `X-ClickHouse-Key`,不要加 query `user` / `password`. reqwest 不会替你加 ClickHouse 头.

reqwest 校验服务端证书的 SAN,不认「只有 CN」. dctl 服务端证书已带 127.0.0.1 与 ::1,与此相符. issue 2941 记录过 0.13 rustls 上 `add_root_certificate` 对自定义根报 `UnknownIssuer`、0.12 却能过;那正是平台校验器与「只使用给定根」的差别,所以证书面用 `tls_certs_only`,不要用 deprecated 的合并路径.

置信: 源码实证(本机 cargo registry 的 reqwest 0.13.3 `tls.rs` 与 `async_impl/client.rs`,以及本仓 Cargo.toml)加 issue 口径(2941).

## 8. PG 与 FK 是否有更优官方形

结论: 两腿都不要改.

Postgres 官方 `cert` 方法就是 `trust` 加上 `clientcert=verify-full`,再在旁边写 `clientcert` 是冗余. 现行 `pg_hba.conf` 文档里 `clientcert` 只有 `verify-ca` 与 `verify-full`. 历史值 `clientcert=1` 是旧的 verify-ca 别名,不核对 CN,比现在更弱,不能加回去. `clientname` 默认 `CN`,另一值是 `DN`. dctl 现写的 `hostssl all all all cert` 就是这个方法;客户端 `PGSSLMODE=verify-full` 核的是服务端证书名字,和 hba 的客户端认证是两侧,都该留着.

FalkorDB 这条是 Redis 的 `tls-auth-clients-user`. 官方示例(redis unstable 的 `TLS.md` 与 `redis.conf` 注释,PR 14610)就是 `CN`. 取值只有 `CN` 与 `off`,默认 `off`. `CN` 表示取出证书 Common Name,按精确字符串找同名 ACL 用户;找不到则作为未认证的 default 用户连上. 它不是用户名参数,也没有 SAN 形. 未设置时,TLS 端口默认要求出示能被 CA 验证的客户端证书(`no` 拒绝客户端证书,`optional` 只在出示时校验);cluster bus 另说,与本 CLI 无关. 本单抽到的 Redis 7.4 与 8.2 `redis.conf` 正文没有这条指令,它新于这两份文档. dctl 现写的 `--tls-auth-clients-user CN`,且客户端证书 CN 等于用户名,已经是官方形.

置信: 官方文档实证(Postgres current 的 cert 与 pg_hba;Redis unstable 的 TLS.md 与 conf 注释).

## 对 dctl ClickHouse 腿的实施形

口令面维持现状: 继续设 `CLICKHOUSE_USER` / `CLICKHOUSE_PASSWORD` / `CLICKHOUSE_DB`,让 entrypoint 写口令用户. 证书面另走下面这套,不要两套 env 叠在同一次启动上.

`config.d` 增加(端口号沿用产品已经发布的 HTTP 口与 9440,不要为了贴官方示例去改发布端口):

- `https_port` 指到现有发布 HTTP 口;`tcp_port_secure` 为 9440.
- 明文 `http_port` 与 `tcp_port` 挪到未发布的冷端口,先不要删键. 删掉 `tcp_port` 会让 entrypoint 的 init 客户端拿到空端口.
- `openSSL/server`: `certificateFile`、`privateKeyFile`、`caConfig`(dctl CA)、`verificationMode` 为 `strict`、`loadDefaultCAFile` 为 false、`cacheSessions` 为 true、`disableProtocols` 为 `sslv2,sslv3`、`preferServerCiphers` 为 true.

`users.d/zz-dctl-user.xml` 在容器启动前就挂上,内容形如(用户名换成真实用户;不要加 password,不要加 `<cn>`):

```xml
<clickhouse>
  <users>
    <default replace="replace">
      <ssl_certificates>
        <common_name>USER</common_name>
      </ssl_certificates>
      <networks>
        <ip>::/0</ip>
      </networks>
      <profile>default</profile>
      <quota>default</quota>
      <access_management>0</access_management>
    </default>
  </users>
</clickhouse>
```

`access_management` 取 0,与今天 entrypoint 口令用户一致. 不写 `grants`: 镜像 entrypoint 的口令用户也不写,主 users.xml 里的 grants 示例是注释. 若实施时 `CREATE DATABASE` 被权限拒绝,再补 grants,那是下一轮实测,不是本单已证事实.

证书面 env: 不传 `CLICKHOUSE_PASSWORD`,不传 `CLICKHOUSE_DB`,`CLICKHOUSE_USER` 不传或保持 `default`. 不用 `CLICKHOUSE_SKIP_USER_SETUP`. 就绪后由 HTTPS 证书客户端执行 `CREATE DATABASE`.

HTTPS 客户端用第 7 项的 `tls_certs_only` 加 `Identity::from_pem`(私钥 PEM 与证书 PEM 拼成一块),并发送第 3 项的两个头. 容器内 REPL 用第 5 项的 `--secure --config`,不要找 `--cafile`.

明确不做: 不改 PG 的 `hostssl ... cert`,不加 `clientcert=1`;不改 FK 的 `--tls-auth-clients-user CN`;证书面不引入 `auth_methods`,除非以后另有「一口令一证书并存」的需求裁定.

## 来源

- 证书用户 XML 与 curl 头: https://clickhouse.com/docs/operations/external-authenticators/ssl-x509
- 证书用户指南(strict、原生口令被忽略、HTTP 头示例): https://clickhouse.com/docs/guides/sre/user-management/ssl-user-auth
- verificationMode 表: https://clickhouse.com/docs/guides/sre/tls/configuring-tls
- openSSL 服务端设置: https://clickhouse.com/docs/operations/server-configuration-parameters/settings#openssl
- 配置合并、字典序、replace/remove: https://clickhouse.com/docs/operations/configuration-files
- ClickHouse Private 对 strict 的不同建议(不要套用): https://clickhouse.com/docs/cloud/clickhouse-private/explanation/pki-and-mtls
- 扁平 XML 不能多认证法: https://github.com/ClickHouse/ClickHouse/issues/86571
- 原生协议 23.3 起出示证书即证书认证、无口令回退: https://github.com/ClickHouse/ClickHouse/issues/48974
- HTTP 不带头则走口令、失败形态 403 / 516: https://github.com/ClickHouse/clickhouse-go/issues/1273 与 https://github.com/ClickHouse/clickhouse-go/issues/1630
- tag `v26.8.9.10-lts` 源码: `docker/server/entrypoint.sh` 的 `manage_clickhouse_user` 与 `init_clickhouse_db`;`src/Access/UsersConfigParser.cpp`;`src/Access/Authentication.cpp` 的 `checkSSLCertificateAuthentication`;`src/Server/HTTP/authenticateUserByHTTP.cpp`. 客户端旗标表读的是 master `programs/client/Client.cpp`.
- reqwest 0.13 Identity::from_pem: https://docs.rs/reqwest/latest/reqwest/tls/struct.Identity.html
- reqwest 0.13 根证书语义与 issue: https://github.com/seanmonstar/reqwest/issues/2941 以及本机 `reqwest-0.13.3` 的 `src/tls.rs`、`src/async_impl/client.rs`. 本仓依赖见 `crates/databasectl/Cargo.toml`.
- Postgres cert 与 clientcert: https://www.postgresql.org/docs/current/auth-cert.html 与 https://www.postgresql.org/docs/current/auth-pg-hba-conf.html
- Redis tls-auth-clients-user: https://github.com/redis/redis/blob/unstable/TLS.md 与 https://github.com/redis/redis/pull/14610
- dctl 现状(只读,未改): `crates/databasectl/src/local/docker.rs` 的 CH env、PG hba `hostssl all all all cert`、FK `--tls-auth-clients-user CN`;`crates/databasectl/src/local/ca.rs` 的客户端 CN 与服务端 SAN.
