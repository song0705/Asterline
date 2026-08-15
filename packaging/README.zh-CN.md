# 第三方包定义

[English](README.md)

此目录保存供包管理器维护者使用的、版本固定的包定义。它们都会安装完整命令
`asterline` 和短命令 `ast`。Homebrew Formula 镜像已发布的 Homebrew tap；AUR 定义会在
该外部服务可用时准备提交。

只有当 Release 的版本化资产、校验和与 provenance 都已可用时，才能推进外部包。不同的
包管理器可以有意停留在不同版本。

## Homebrew（macOS 和 Linux）

`homebrew/Formula/asterline.rb` 通过 SHA-256 固定 v0.2.8 的 macOS 与 GNU/Linux 归档：
Apple silicon / Intel macOS，以及 ARM64 / x86_64 Linux。Linux 归档是已验证的 glibc 2.28
基线 Release 资产。

用户安装命令：

```bash
brew install song0705/asterline/asterline
```

使用以下命令验证已发布的 Formula：

```bash
brew tap song0705/asterline
brew audit --strict --online --formula song0705/asterline/asterline
brew install --formula song0705/asterline/asterline
brew test song0705/asterline/asterline
```

## Arch User Repository

`aur/asterline` 是为 `x86_64` 和 `aarch64` 准备的稳定源码包。它有意保持在 v0.2.3，
不会随 Homebrew 自动升级。它固定精确 commit 与 SHA-256，并在构建前断言源码 manifest
报告预期的包版本。v0.2.3 tag 虽是 annotated，但没有 GPG 签名，因此该包固定解析出的
release commit，而不是信任可移动的 tag 引用。

相比 v0.2.3 的 Linux 二进制归档，更推荐源码包：这些历史归档要求 `GLIBC_2.39` 且动态
链接 `libsqlite3`。AUR 定义会针对目标 Arch 系统编译，并明确声明运行时 SQLite 依赖。

将此目录复制到 `asterline` AUR Git 仓库，然后在 Arch 上推送前验证并重新生成 metadata：

```bash
cd asterline
makepkg --verifysource
makepkg --syncdeps --cleanbuild
makepkg --printsrcinfo > .SRCINFO
git diff --exit-code -- .SRCINFO
```

发布后，用户可通过 `yay -S asterline`（或其他 AUR helper）安装。

`makepkg --verifysource` 可离线重复校验已下载的源码归档。干净 chroot 内的 Cargo 构建仍需
registry 访问，因为上游 Release 还未发布 vendored dependency archive。

## Linux Release 包

自 v0.2.6 起，每种 Linux 架构都使用一致、可见的资产前缀，参考成熟的
`Linux-arm64` / `Linux-x86_64` 布局：

| CPU 架构 | 便携归档 | Debian / Ubuntu | RPM Linux |
| --- | --- | --- | --- |
| ARM64 | `asterline-v<version>-Linux-arm64.tar.gz` | `asterline-v<version>-Linux-arm64.deb` | `asterline-v<version>-Linux-arm64.rpm` |
| Intel / AMD 64 位（`x86_64`） | `asterline-v<version>-Linux-x86_64.tar.gz` | `asterline-v<version>-Linux-x86_64.deb` | `asterline-v<version>-Linux-x86_64.rpm` |

workflow 从已验证的 GNU/Linux 归档构建 Debian 包，用 `dpkg-shlibdeps` 推导运行时依赖，
随后在 Debian 12 和 Ubuntu 24.04 安装、运行、移除每一个包。它在 Rocky Linux 8 中从同一
已验证归档构建 RPM，保留自动 RPM 共享库依赖，再在 Fedora 44 重复安装、执行、移除。
它们是 GitHub Release 资产而非 APT 或 DNF 仓库；未来仓库需有自己的签名密钥和托管。

从包含对应资产的 Release 下载匹配文件后，可用以下命令安装：

```bash
sudo apt install ./asterline-v<version>-Linux-x86_64.deb
sudo dnf install ./asterline-v<version>-Linux-x86_64.rpm
ast --help
```

现有 v0.2.3 Release 早于所有 Linux 包资产；v0.2.5 包含较早的仅 Debian 双架构包；
v0.2.7 是首个使用可见架构命名并提供对应 RPM 包的 Release。

## Release 更新清单

每次发布 Asterline Release 后，更新 Formula 版本、归档 URL 和 SHA-256。对于 AUR 包，
将 annotated tag 解析为 commit，固定该 commit 及其 archive SHA-256，更新
`pkgver` / `pkgrel`，再用 `makepkg` 重新生成 `.SRCINFO`。Debian 与 RPM 包名和校验和由
release workflow 生成。推送任何外部包仓库前，运行上面的验证程序。版本化 Release 归档
不得使用 `releases/latest` 或 `SKIP`。
