param(
    [string]$Version = "",

    [string]$TargetDirectory = "target\x86_64-pc-windows-msvc\release",

    [string]$OutputDirectory = "dist"
)

$ErrorActionPreference = "Stop"

if (-not $Version) {
    $metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
    $package = $metadata.packages | Where-Object { $_.name -eq "asterline" } | Select-Object -First 1
    if (-not $package) {
        throw "Could not find the asterline package in Cargo metadata."
    }
    $Version = $package.version
}

$compiler = Get-Command "ISCC.exe" -ErrorAction SilentlyContinue
if (-not $compiler) {
    $candidates = @(
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    )
    $compilerPath = $candidates | Where-Object { Test-Path $_ } | Select-Object -First 1
    if (-not $compilerPath) {
        throw "Inno Setup 6 was not found. Install it or add ISCC.exe to PATH."
    }
} else {
    $compilerPath = $compiler.Path
}

$sourceDirectory = (Resolve-Path $TargetDirectory).Path
foreach ($binary in @("asterline.exe", "ast.exe")) {
    if (-not (Test-Path (Join-Path $sourceDirectory $binary))) {
        throw "Missing release binary: $(Join-Path $sourceDirectory $binary)"
    }
}

New-Item -ItemType Directory -Force $OutputDirectory | Out-Null
$outputDirectoryPath = (Resolve-Path $OutputDirectory).Path
$scriptPath = (Resolve-Path "packaging\windows\asterline.iss").Path

& $compilerPath `
    "/DMyAppVersion=$Version" `
    "/DSourceDir=$sourceDirectory" `
    "/O$outputDirectoryPath" `
    $scriptPath

if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed with exit code $LASTEXITCODE."
}

$installer = Join-Path $outputDirectoryPath "asterline-$Version-x86_64-windows-setup.exe"
if (-not (Test-Path $installer)) {
    throw "Inno Setup did not produce the expected installer: $installer"
}

Write-Output $installer
