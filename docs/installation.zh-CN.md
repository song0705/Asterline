# 安装 Asterline

Asterline 为 macOS、Linux 和 Windows 提供原生发布包。每个发布包都同时包含完整命令 `asterline` 和短命令 `ast`。

## 安装前准备

Asterline 用于协调各提供商的 CLI，并不代替这些 CLI。创建团队前，请至少安装并登录 `codex`、`claude`、`grok` 或 `agy` 中的一个。只有从源码构建 Asterline 时才需要 Rust。

## macOS

通用 DMG 同时支持 Apple silicon 和 Intel Mac。

1. 打开[最新版本](https://github.com/song0705/Asterline/releases/latest)。
2. 下载 `asterline-<version>-macos-universal.dmg`。
3. 打开 DMG，双击 `Install Asterline.pkg`。
4. 按照 macOS 标准安装器完成安装。
5. 打开新的终端窗口并验证：

```bash
ast --help
```

安装包会把 `ast` 和 `asterline` 安装到 `/usr/local/bin`。

### macOS 便携包

不适合进行系统级安装时，可以改用便携包：

| Mac           | 发布目标               |
| ------------- | ---------------------- |
| Apple silicon | `aarch64-apple-darwin` |
| Intel         | `x86_64-apple-darwin`  |

解压对应的 `.tar.gz`，然后为当前用户安装两个命令：

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 asterline "$HOME/.local/bin/asterline"
install -m 755 ast "$HOME/.local/bin/ast"
```

如果新终端仍然找不到 `ast`，请在 `~/.zprofile` 中把 `$HOME/.local/bin` 加入 `PATH`。

### macOS 安全提示

使用 Developer ID 签名并通过公证的版本可以直接进入标准安装流程。对于未签名的预览构建，macOS 可能要求用户明确允许：

1. 按住 Control 点击 `Install Asterline.pkg`，选择“打开”。
2. 在安全提示中再次确认“打开”。
3. 如果没有出现允许选项，请打开“系统设置 → 隐私与安全性”，在其中批准该安装包。

只有从 Asterline 官方 Release 页面下载并核对校验和后，才应绕过这项提示。

### 在 macOS 上卸载

删除已安装的命令和安装器收据：

```bash
sudo rm /usr/local/bin/ast /usr/local/bin/asterline
sudo pkgutil --forget io.github.song0705.asterline
```

如果使用便携安装，只需删除 `$HOME/.local/bin` 中的两个文件。

## Windows

Windows 安装器支持 64 位 Windows 10 和 11。

1. 打开[最新版本](https://github.com/song0705/Asterline/releases/latest)。
2. 下载 `asterline-<version>-x86_64-windows-setup.exe`。
3. 使用默认选项完成安装。
4. 打开新的 PowerShell 窗口并验证：

```powershell
ast --help
```

安装器会为当前用户安装 Asterline，将安装目录加入用户 `Path`，并注册标准卸载入口。

### Windows 自动更新

通过安装器管理的版本每 24 小时最多检查一次最新稳定 Release。执行更新前，Asterline 会使用同一 Release 中的 `SHA256SUMS` 校验新安装器；当前 Asterline 退出后，安装器才会替换程序文件。

立即检查更新：

```powershell
ast --update
```

单次启动跳过后台检查：

```powershell
ast --no-auto-update
```

便携 ZIP 和源码构建版本不会自动更新。

### Windows 便携 ZIP

下载 `asterline-<version>-x86_64-pc-windows-msvc.zip`，解压后运行：

```powershell
.\ast.exe --help
```

便携包不会修改 `Path`，也不会注册卸载入口。

### 在 Windows 上卸载

打开“设置 → 应用 → 已安装的应用”，找到 Asterline 并选择“卸载”。安装器会同时移除它添加的用户 `Path` 条目。便携版只需删除解压目录。

## Linux

根据机器架构下载对应的 `.tar.gz`：

| 架构               | 发布目标                    |
| ------------------ | --------------------------- |
| Intel 或 AMD 64 位 | `x86_64-unknown-linux-gnu`  |
| ARM64              | `aarch64-unknown-linux-gnu` |

解压后，为当前用户安装两个命令：

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 asterline "$HOME/.local/bin/asterline"
install -m 755 ast "$HOME/.local/bin/ast"
```

如有需要，请把 `$HOME/.local/bin` 加入 shell 的 `PATH` 配置，然后打开新 shell 并运行 `ast --help`。

卸载 Linux 便携版时，删除 `$HOME/.local/bin/ast` 和 `$HOME/.local/bin/asterline` 即可。

## 从源码构建

安装 Rust 1.85 或更高版本，克隆仓库后运行：

```bash
cargo install --path . --locked --force
```

Cargo 会把两个命令安装到 macOS/Linux 的 `$HOME/.cargo/bin`，或 Windows 的 `%USERPROFILE%\.cargo\bin`。请确认对应目录已经加入 `PATH`。

## 校验发布包

每个 Release 都会在安装器和便携包旁提供 `SHA256SUMS` 与 GitHub 构建来源证明。

Linux 用户可把 `SHA256SUMS` 与安装包下载到同一目录，然后运行：

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

macOS 用户可计算已下载文件的摘要：

```bash
shasum -a 256 asterline-<version>-macos-universal.dmg
```

Windows PowerShell 用户可运行：

```powershell
Get-FileHash .\asterline-<version>-x86_64-windows-setup.exe -Algorithm SHA256
```

macOS 和 Windows 用户需要把结果与 `SHA256SUMS` 中同名文件的记录进行比较。

## 故障排查

### 找不到 `ast`

安装后请先打开新的终端。如果便携安装仍然无法找到命令，请确认安装目录已经加入 `PATH`。

### 首次启动时没有可用后端

请在同一个终端里直接运行提供商命令，例如 `codex --version`。如果该命令失败，请先安装或登录对应 CLI，然后重新启动 Asterline。

### 无法写入项目状态

Asterline 会在所选工作区中创建 `.asterline/`。请确认工作区可写，或通过 `--workspace` 指定其他目录。
