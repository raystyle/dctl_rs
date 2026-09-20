---
id: ADR-0004
title: 分发只走 GitHub Releases,砍掉 crates.io/npm/PyPI 渠道
status: accepted
date: 2026-09-20
deciders: [ray]
supersedes: []
superseded_by: null
tags: [release, distribution]
---

# 分发只走 GitHub Releases

## Context

上游一版四渠道同发(crates.io token、npm OIDC、PyPI OIDC、GitHub Releases 镜像到 builds.clickhouse.com CDN)。fork 没有 ClickHouse 的 OIDC trusted publishing 身份与 CDN;四渠道 lockstep 版本纪律(三处版本号同步)是纯负担。用户群短期内是自团队。

## Decision

分发收缩为 GitHub Releases 单点:release workflow 保留构建矩阵(musl 静态产物 file/ldd 双验)、8 发行版 smoke 测试、gh release create;删除 publish-crates/publish-npm/build-wheels/publish-pypi 四个 job 与 npm/、pypi/ 目录。install.sh 与自更新(update.rs)及 binstall pkg-url 全部改指 `raystyle/dctl_rs` 的 release 资产,归档命名沿用上游方案 `dctl-{target}-v{version}.tar.gz`。版本只需 bump 单处 Cargo.toml。

## Consequences

- 好面:发布面最小,版本纪律从三处收缩到一处;自更新链路指向自己 [实证: 2026-09-20 install.sh 与 RELEASES_BASE_URL 改指后 URL 拼接与归档命名核对一致]
- 坏面:用户少了 cargo install/crates.io 途径;未来要回补 npm/PyPI 时 OIDC trusted publishing 需在各自平台重新配置,release.yml 里上游写法可从 git 历史找回
- 注意:dctl 在 crates.io 未被占用,回补时可直接注册 [实证: 2026-09-20 crates.io API 404]
