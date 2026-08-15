# 发布 Asterline

[English](releasing.md)

Asterline 由 GitHub Actions 构建和发布。Release tag 必须与 `Cargo.toml` 中的包版本精确
匹配。常规 workflow 和 release workflow 使用 Rust 1.93.1；显式 MSRV job 强制包声明的
最低 Rust 1.88。

下一次发布前，仓库管理员必须在 **Settings → General → Releases** 启用
**Enable release immutability**。GitHub 说明该设置只保护未来 Release，不影响既有 Release。
它会锁定已发布 Release 的 tag 和资产，因此 workflow 会先创建 draft，上传并检查所有资产，
最后才发布。参阅 GitHub 的
[不可变 Release 指引](https://docs.github.com/en/code-security/how-tos/secure-your-supply-chain/establish-provenance-and-integrity/prevent-release-changes)。

## 准备发布

1. 更新 `Cargo.toml` 的 `version`。
2. 运行 `cargo check`，让 `Cargo.lock` 记录包版本。
3. 一次性安装 CI 固定的 audit 工具，然后运行可移植的本地质量门：

   ```bash
   cargo install cargo-audit --version 0.22.2 --locked
   just check
   ```

   `just check` 覆盖格式化、无 warning 的 Clippy、全部 target 测试和依赖 audit。平台和
   安装器 job 仍只在 Actions 中执行。

4. 新增 `docs/releases/v<version>.md`，写面向用户的摘要。文件不存在时 workflow 会回退到
   GitHub 生成的 release notes。该说明必须提供中文和英文版本，或在文档内链接到对应语言版本。
5. 读取包版本、获取既有 tag，并确认 release tag 未被使用：

   ```bash
   version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
   git fetch --tags
   test -z "$(git tag --list "v${version}")"
   ! git ls-remote --exit-code --tags origin "refs/tags/v${version}" >/dev/null 2>&1
   ! gh release view "v${version}" --repo song0705/Asterline >/dev/null 2>&1
   ```

6. 提交并推送版本变更和 release notes，**不要**带 tag。
7. 等待该精确 commit 的常规 CI 在 Linux、macOS 和 Windows 全部通过。任何必需 job 仍在等待
   或失败时都不要创建 release tag：

   ```bash
   commit="$(git rev-parse HEAD)"
   run_id="$(gh run list --workflow CI --commit "$commit" --limit 1 \
     --json databaseId --jq '.[0].databaseId')"
   test -n "$run_id"
   gh run watch "$run_id" --exit-status
   ```

8. 仅当该 commit 全绿后，从它创建并推送 annotated tag：

   ```bash
   version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
   git tag -a "v${version}" -m "Asterline v${version}"
   test "$(git cat-file -t "refs/tags/v${version}")" = tag
   test "$(git rev-parse "refs/tags/v${version}^{commit}")" = "$(git rev-parse HEAD)"
   git push origin main "v${version}"
   ```

## 自动发布

推送 tag 会启动 `.github/workflows/release.yml`。workflow 会：

1. 验证 annotated tag 与 Cargo 版本匹配、解析到触发 commit、包含在 `origin/main` 中，且没有
   已发布 Release；
2. 在 Linux 执行格式化和无 warning Clippy，然后在 Linux、macOS、Windows 运行测试套件；
3. 在按 digest 固定且受支持的 PyPA `manylinux_2_28` 容器内构建 Linux x86-64 和 ARM64，
   并在原生 runner 构建 macOS Intel、macOS Apple silicon、Windows x86-64 MSVC；
4. 将 Unix target 打成便携 `.tar.gz`，Windows 打成便携 `.zip`，并为每个已验证 GNU/Linux
   架构生成便携归档、Debian 包和 RPM 包，统一使用可见的 `Linux-arm64` 或 `Linux-x86_64`
   前缀；
5. 在 Debian 12 与 Ubuntu 24.04 安装、运行、删除每个 Debian 包，在 Rocky Linux 8 与
   Fedora 44 对每个 RPM 包做同样验证，才允许发布；
6. 把 Intel 和 Apple silicon 二进制合并为一个通用 macOS DMG，其中包含安装到
   `/usr/local/bin` 的原生 `Install Asterline.pkg`，并构建每用户 Windows Setup `.exe`；
7. 安装生成的 Windows Setup、运行 `ast --help`、验证 `/WAITPID` 更新等待，并卸载后才允许
   发布；
8. 在拒绝任何遗漏或意外资产后，为每个归档和安装器创建 `SHA256SUMS` 与已签名 GitHub
   artifact attestation；
9. 删除此前尝试遗留的不完整 draft，但不删除或移动 tag；使用
   `docs/releases/<tag>.md`（或 generated notes）创建干净 draft，上传全部资产，将已上传
   资产名与 `dist` 比较，然后才发布完整 draft。

`publish` job 会记录 `quality` 已验证的 annotated-tag object。每次修改 draft 前、以及发布
前，它都会通过 GitHub Git Data API 查询远端 ref 与 tag object，要求完全相同的 tag-object
SHA，并要求该 object 指向触发 `GITHUB_SHA`。因此在较慢的打包 job 运行期间移动 tag 会
fail closed。

Draft Release 有意可重建。运行在 draft 创建或 asset 上传中失败时，重新运行同一已打 tag
workflow：当所有 build、smoke、checksum、attestation gate 再次通过后，`publish` 只删除该
draft 并干净重建。它不会传 `--cleanup-tag`，并在删除 draft 后验证远端 annotated-tag object
未变。已发布 Release 从不替换；应发布新版本。Build artifact 上传也使用 Action 的显式
`overwrite` mode，因此完整 workflow 重跑会替换失败尝试的 artifact，不会与名称冲突。

macOS job 总会验证 DMG、挂载它、展开 package payload、检查两种 Mach-O 架构，并运行已安装
布局下的 `ast --help`。当以下所有仓库 secret 都存在时，它会 Developer ID 签名二进制、
package、DMG，然后公证并 staple：

- `MACOS_CERTIFICATE_P12_BASE64`
- `MACOS_CERTIFICATE_PASSWORD`
- `MACOS_APPLICATION_IDENTITY`
- `MACOS_INSTALLER_IDENTITY`
- `APPLE_NOTARY_KEY_P8_BASE64`
- `APPLE_NOTARY_KEY_ID`
- `APPLE_NOTARY_ISSUER_ID`

P12 必须包含所列 Developer ID Application 与 Developer ID Installer identity。缺少任一
secret 时，workflow 会输出 warning 并发布未签名、未公证 DMG；其二进制仅 ad-hoc 签名。
`SHA256SUMS` 和 GitHub artifact attestation 仍覆盖该文件，但不能替代 Developer ID 签名或
Apple 公证。

Linux 归档有意使用 `*-unknown-linux-gnu`，而不是 musl。release workflow 使用 PyPA 维护的
`manylinux_2_28` 镜像，为 x86-64 和 ARM64 提供 glibc 2.28 构建基线。构建后脚本会拒绝要求
更高 GLIBC symbol 或动态链接 `libsqlite3` 的二进制；`rusqlite` 使用跨平台 `bundled`
feature。因此归档要求 glibc 2.28 或更新、内置 SQLite，不支持 Alpine/musl。镜像引用按
digest 固定；PyPA 发布维护的替代项时需审查并更新两个 digest。参阅 PyPA 的
[受支持镜像与 ABI 矩阵](https://github.com/pypa/manylinux#manylinux_2_28-almalinux-8-based)。

GNU/Linux 归档通过该门后，`package-debian` 会在按 digest 固定的 Debian 12 容器中解包每份
归档，用 `dpkg-shlibdeps` 推导运行时 `Depends`，构建
`asterline-v<version>-Linux-arm64.deb` 或 `asterline-v<version>-Linux-x86_64.deb`。它会在
该处安装、执行、purge 每个包，再由 `smoke-deb-ubuntu` 在原生 Ubuntu 24.04 runner 重复测试。

`package-rpm` 在按 digest 固定的 Rocky Linux 8 容器中，从同一已验证归档打成
`asterline-v<version>-Linux-arm64.rpm` 或
`asterline-v<version>-Linux-x86_64.rpm`。RPM spec 保留自动共享库依赖并拒绝系统 SQLite
依赖。随后 `smoke-rpm-fedora` 在全新的按 digest 固定 Fedora 44 容器中安装、执行、移除每份
资产。生成的 `.deb` 和 `.rpm` 只是 Release 资产；在另外管理签名密钥和仓库前，不要把它们
描述为 APT 或 DNF 仓库。

Windows build 在 `windows-latest` 上运行，并链接 bundled SQLite source，因此不依赖 runner
或用户安装的 `sqlite3.lib`。Inno Setup 根据 `packaging/windows/asterline.iss` 构建安装器。
常规 Windows CI 与 release workflow 都会调用 `scripts/smoke-windows-installer.ps1`：安装到
临时目录、运行 `ast --help`、验证用户 `Path`、测量普通更新的无等待基线、证明 `/WAITPID`
超过该窗口仍被阻塞、卸载并确认清理。`publish` 显式依赖 release smoke job。Release gate 和
常规 CI 也在 Windows 运行 `cargo test --all-targets --locked`；要保留真实的链接与执行 job，
不要用 `cargo check` 替换。

真实提供商兼容性是独立、付费、手动批准的 gate。使用
`.github/workflows/real-smoke.yml` 并遵循[真实冒烟 runner 与凭据指南](real-smoke.zh-CN.md)；
它有意不是 pull-request job。

在命令行监控发布：

```bash
gh run list --workflow Release
gh run watch --exit-status
```

Workflow action 按完整 commit SHA 固定。Rust 的正常 toolchain 也固定为 1.93.1，而不是会
漂移的 `stable` channel；MSRV job 固定为 1.88.0。接受更新时审查 release notes，并在每个
SHA 旁保留人类可读 Action 版本注释。Rust toolchain 和 manylinux digest 更新仍是有意的维护者
工作。

不要移动或复用**已发布**版本的 tag。修复问题后提升版本并发布新 tag。Release 以前的失败
workflow 可以在 tag 不变时重跑；不得通过新 tag 来绕过一个尚未发布的失败 job。

## 历史 provenance 说明：v0.2.2

成功的 [v0.2.2 release workflow
run](https://github.com/song0705/Asterline/actions/runs/31376054346) 记录的 source commit 是
`61b56740606ec3ab52e423b3dcc4b1377babe461`。公开 `v0.2.2` tag 当前解析为
`c4be080788be4187b9daff91c561ecbd68f4347e`。它们具有相同 Git tree
`d99f356a23a2d4b38dc0045759a159db3de23816`，但 commit identity 不同。这是历史 provenance
异常，不是再次通过移动 tag 修复历史的许可。保留此记录，远端 tag 保持不动，未来的每个
修正都使用新版本。Release immutability 仅保护管理员启用后发布的 Release，不能追溯修复
v0.2.2。
