param(
  [Parameter(Mandatory = $true)]
  [string]$InstallerPath,
  [string]$InstallDir = (Join-Path ([IO.Path]::GetTempPath()) "AsterlineInstallerSmoke-$PID")
)

$ErrorActionPreference = "Stop"
$installer = (Resolve-Path $InstallerPath).Path

if (Test-Path $InstallDir) {
  throw "Refusing to reuse existing smoke-test directory: $InstallDir"
}

$install = Start-Process -FilePath $installer -ArgumentList @(
  "/VERYSILENT",
  "/SUPPRESSMSGBOXES",
  "/NORESTART",
  "/DIR=$InstallDir"
) -Wait -PassThru
if ($install.ExitCode -ne 0) {
  throw "Installer exited with $($install.ExitCode)."
}

foreach ($file in @("asterline.exe", "ast.exe", ".asterline-installer-managed", "unins000.exe")) {
  if (-not (Test-Path (Join-Path $InstallDir $file))) {
    throw "Installer did not create $file."
  }
}

& (Join-Path $InstallDir "ast.exe") --help | Out-Null
if ($LASTEXITCODE -ne 0) {
  throw "Installed ast.exe --help failed."
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not (($userPath -split ";") -contains $InstallDir)) {
  throw "Installer did not add its directory to the user Path."
}

$baselineWatch = [Diagnostics.Stopwatch]::StartNew()
$baselineUpdate = Start-Process -FilePath $installer -ArgumentList @(
  "/VERYSILENT",
  "/SUPPRESSMSGBOXES",
  "/NORESTART",
  "/DIR=$InstallDir"
) -Wait -PassThru
$baselineWatch.Stop()
if ($baselineUpdate.ExitCode -ne 0) {
  throw "Baseline update installer exited with $($baselineUpdate.ExitCode)."
}

$blocker = Start-Process -FilePath "pwsh" -ArgumentList @(
  "-NoProfile",
  "-Command",
  "Start-Sleep -Seconds 300"
) -PassThru
$update = Start-Process -FilePath $installer -ArgumentList @(
  "/VERYSILENT",
  "/SUPPRESSMSGBOXES",
  "/NORESTART",
  "/DIR=$InstallDir",
  "/WAITPID=$($blocker.Id)"
) -PassThru

$probeMilliseconds = [int][Math]::Max(
  5000,
  [Math]::Min(60000, [Math]::Ceiling($baselineWatch.Elapsed.TotalMilliseconds * 3 + 2000))
)
if ($update.WaitForExit($probeMilliseconds)) {
  if (-not $blocker.HasExited) {
    $blocker.Kill()
    $blocker.WaitForExit()
  }
  throw "Update installer exited within the no-wait baseline window; /WAITPID was not honored."
}
if ($blocker.HasExited) {
  throw "The blocker exited before the /WAITPID assertion completed."
}
$blocker.Kill()
$blocker.WaitForExit()
if (-not $update.WaitForExit(15000)) {
  $update.Kill()
  throw "Update installer did not finish after the requested process exited."
}
if ($update.ExitCode -ne 0) {
  throw "Update installer exited with $($update.ExitCode)."
}

$uninstall = Start-Process -FilePath (Join-Path $InstallDir "unins000.exe") -ArgumentList @(
  "/VERYSILENT",
  "/SUPPRESSMSGBOXES",
  "/NORESTART"
) -Wait -PassThru
if ($uninstall.ExitCode -ne 0) {
  throw "Uninstaller exited with $($uninstall.ExitCode)."
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ";") -contains $InstallDir) {
  throw "Uninstaller left its directory on the user Path."
}
if (Test-Path (Join-Path $InstallDir "ast.exe")) {
  throw "Uninstaller left ast.exe behind."
}

Write-Host "Windows installer smoke test passed: install, --help, /WAITPID update, uninstall"
