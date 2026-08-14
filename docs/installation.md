# Installing Asterline

Asterline publishes native packages for macOS, Linux, and Windows. Every release
contains both the full `asterline` command and the shorter `ast` alias.

## Before you install

Asterline coordinates provider CLIs; it does not replace them. Install and
authenticate at least one of `codex`, `claude`, `grok`, or `agy` before creating
a team. Rust is required only when building Asterline from source.

## macOS

The universal DMG supports both Apple silicon and Intel Macs.

1. Open the
   [latest Release](https://github.com/song0705/Asterline/releases/latest).
2. Download `asterline-<version>-macos-universal.dmg`.
3. Open the DMG and double-click `Install Asterline.pkg`.
4. Complete the standard macOS Installer flow.
5. Open a new Terminal window and verify the installation:

```bash
ast --help
```

The package installs `ast` and `asterline` into `/usr/local/bin`.

### Portable macOS archive

Use a portable archive when a system-wide installation is not appropriate:

| Mac           | Release target         |
| ------------- | ---------------------- |
| Apple silicon | `aarch64-apple-darwin` |
| Intel         | `x86_64-apple-darwin`  |

Extract the matching `.tar.gz`, then install the commands for the current user:

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 asterline "$HOME/.local/bin/asterline"
install -m 755 ast "$HOME/.local/bin/ast"
```

If a new terminal cannot find `ast`, add `$HOME/.local/bin` to `PATH` in
`~/.zprofile`.

### macOS security prompt

The v0.2.3 DMG is a historical exception: it was published unsigned before the
fail-closed signing policy. Stable Releases produced by the hardened workflow
after v0.2.3 must be Developer ID signed and notarized; the workflow fails
instead of publishing a DMG when either credential set is unavailable. Verify
v0.2.3's checksum before using the security override. A locally built, unsigned
preview may also require an explicit override:

1. Control-click `Install Asterline.pkg` and choose **Open**.
2. Confirm **Open** in the security dialog.
3. If no override is offered, open **System Settings → Privacy & Security** and
   approve the package there.

Only bypass this warning for an artifact downloaded from the official Asterline
Release page after verifying its checksum.

### Uninstall on macOS

Remove the installed commands and forget the Installer receipt:

```bash
sudo rm /usr/local/bin/ast /usr/local/bin/asterline
sudo pkgutil --forget io.github.song0705.asterline
```

For a portable installation, remove the two files from `$HOME/.local/bin`
instead.

## Windows

The Windows installer supports 64-bit Windows 10 and 11.

1. Open the
   [latest Release](https://github.com/song0705/Asterline/releases/latest).
2. Download `asterline-<version>-x86_64-windows-setup.exe`.
3. Run Setup with the default options.
4. Open a new PowerShell window and verify the installation:

```powershell
ast --help
```

Setup installs Asterline for the current user, adds its directory to the user
`Path`, and registers an uninstaller.

### Automatic updates on Windows

Setup-managed installations check the latest stable Release at most once every
24 hours. Before an update runs, Asterline verifies the downloaded installer
against that Release's `SHA256SUMS`. The installer waits for Asterline to exit
before replacing the binaries.

Run an update check immediately:

```powershell
ast --update
```

Skip the background check for one launch:

```powershell
ast --no-auto-update
```

Portable ZIP copies and source builds do not update themselves.

### Portable Windows ZIP

Download `asterline-<version>-x86_64-pc-windows-msvc.zip`, extract it, and run:

```powershell
.\ast.exe --help
```

The portable package does not modify `Path` or register an uninstaller.

### Uninstall on Windows

Open **Settings → Apps → Installed apps**, find **Asterline**, and choose
**Uninstall**. Setup removes its user `Path` entry. For a portable copy, delete
the extracted directory.

## Linux

Download the `.tar.gz` archive matching the machine:

| Architecture        | Release target              |
| ------------------- | --------------------------- |
| Intel or AMD 64-bit | `x86_64-unknown-linux-gnu`  |
| ARM64               | `aarch64-unknown-linux-gnu` |

These are GNU/Linux builds produced on a maintained glibc 2.28 baseline. They
require glibc 2.28 or newer, so they do not run on Alpine/musl. SQLite is built
from bundled source and does not require a system `libsqlite3` package.

> Historical exception: the existing v0.2.3 Linux archives predate this release
> guarantee. They require glibc 2.39 and dynamically link system `libsqlite3`.
> Use them only with those runtime dependencies; otherwise build from source or
> use a distribution package.

Extract the archive, then install the commands for the current user:

```bash
mkdir -p "$HOME/.local/bin"
install -m 755 asterline "$HOME/.local/bin/asterline"
install -m 755 ast "$HOME/.local/bin/ast"
```

Add `$HOME/.local/bin` to the shell's `PATH` configuration if necessary, then
open a new shell and run `ast --help`.

To uninstall a portable Linux copy, remove `$HOME/.local/bin/ast` and
`$HOME/.local/bin/asterline`.

## Build from source

Install Rust 1.88 or newer, clone the repository, and run:

```bash
cargo install --path . --locked --force
```

Cargo installs both commands into `$HOME/.cargo/bin` on macOS and Linux, or
`%USERPROFILE%\.cargo\bin` on Windows. Ensure that directory is on `PATH`.

## Verify a release

Each Release publishes `SHA256SUMS` and GitHub artifact attestations beside the
installers and portable archives.

On Linux, download `SHA256SUMS` into the same directory as the artifact and run:

```bash
sha256sum --check --ignore-missing SHA256SUMS
```

On macOS, calculate the downloaded artifact's digest with:

```bash
shasum -a 256 asterline-<version>-macos-universal.dmg
```

On Windows PowerShell, use:

```powershell
Get-FileHash .\asterline-<version>-x86_64-windows-setup.exe -Algorithm SHA256
```

Compare the macOS or Windows result with the matching entry in `SHA256SUMS`.

## Troubleshooting

### `ast` is not found

Open a new terminal after installation. If a portable installation is still not
found, confirm that its destination directory is present in `PATH`.

### No backends appear on first launch

Run the provider command directly in the same terminal, for example
`codex --version`. Install or authenticate the provider CLI if that command
fails, then restart Asterline.

### Asterline cannot write project state

Asterline creates `.asterline/` in the selected workspace. Confirm that the
workspace is writable, or pass a different workspace with `--workspace`.
