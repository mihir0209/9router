# 9Router Universal Windows PowerShell Installer
# Supported OS: Windows 10 / 11 / Windows Server (x64)
# Usage:
#   irm https://raw.githubusercontent.com/mihir0209/9router/master/install.ps1 | iex

$ErrorActionPreference = "Stop"

$Repo = "mihir0209/9router"
$CommandPrimary = "9r"
$CommandAlias = "9router"
$DefaultInstallDir = "$env:USERPROFILE\.local\bin"

function Log-Info($msg) {
    Write-Host "> $msg" -ForegroundColor Cyan
}

function Log-Success($msg) {
    Write-Host "[+] $msg" -ForegroundColor Green
}

function Log-Error($msg) {
    Write-Host "x $msg" -ForegroundColor Red
}

# Verify architecture
$is64Bit = [System.Environment]::Is64BitOperatingSystem
if (-not $is64Bit) {
    Log-Error "9Router currently supports 64-bit Windows (x64) only."
    exit 1
}

$InstallDir = $DefaultInstallDir
if (-not (Test-Path -Path $InstallDir)) {
    New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
}

$TargetExe = Join-Path $InstallDir "$CommandPrimary.exe"
$AliasCmd = Join-Path $InstallDir "$CommandAlias.cmd"
$PrimaryCmd = Join-Path $InstallDir "$CommandPrimary.cmd"

Log-Info "Target install location: $TargetExe"

# Resolve latest release version
Log-Info "Fetching latest release information from GitHub..."
$TargetVersion = "latest"
try {
    $ReleaseInfo = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing -ErrorAction SilentlyContinue
    if ($ReleaseInfo -and $ReleaseInfo.tag_name) {
        $TargetVersion = $ReleaseInfo.tag_name
    }
} catch {
    $TargetVersion = "v0.6.0"
}

Log-Info "Target release: $TargetVersion"

$DownloadUrl = "https://github.com/$Repo/releases/download/$TargetVersion/9r-windows-x64.exe"
if ($TargetVersion -eq "latest") {
    $DownloadUrl = "https://github.com/$Repo/releases/latest/download/9r-windows-x64.exe"
}

Log-Info "Downloading 9Router binary from $DownloadUrl..."
$TempFile = Join-Path $env:TEMP "9r-$([Guid]::NewGuid().ToString('N')).exe"

try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TempFile -UseBasicParsing
    Move-Item -Path $TempFile -Destination $TargetExe -Force
} catch {
    Log-Error "Failed to download 9Router binary: $_"
    if (Test-Path $TempFile) { Remove-Item -Force $TempFile }
    exit 1
}

# Create .cmd shims for cmd.exe and PowerShell compatibility
$CmdShimContent = "@echo off`r`n`"%~dp0$CommandPrimary.exe`" %*"

Set-Content -Path $AliasCmd -Value $CmdShimContent -Encoding ASCII
Set-Content -Path $PrimaryCmd -Value $CmdShimContent -Encoding ASCII

Log-Success "Binary installed: $TargetExe"
Log-Success "Shims created: $AliasCmd, $PrimaryCmd"

# Check and update User Environment PATH
$UserPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
$PathParts = $UserPath -split ";"
$InPath = $false

foreach ($p in $PathParts) {
    if ($p.TrimEnd('\') -eq $InstallDir.TrimEnd('\')) {
        $InPath = $true
        break
    }
}

if (-not $InPath) {
    Log-Info "Adding $InstallDir to User Environment PATH..."
    $NewUserPath = if ([string]::IsNullOrWhiteSpace($UserPath)) { $InstallDir } else { "$UserPath;$InstallDir" }
    [System.Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
    $env:Path = "$env:Path;$InstallDir"
    Log-Success "Updated User PATH in Windows Registry."
}

Write-Host ""
Write-Host "9Router ($CommandPrimary) installed successfully!" -ForegroundColor Green
Write-Host ""
Write-Host "Quick start:"
Write-Host "  $CommandPrimary start -d       # Start background daemon" -ForegroundColor Cyan
Write-Host "  $CommandPrimary status         # View running gateway status" -ForegroundColor Cyan
Write-Host "  $CommandPrimary connect        # Auto-configure local AI coding tools" -ForegroundColor Cyan
Write-Host "  $CommandPrimary --help         # Show complete command reference" -ForegroundColor Cyan
Write-Host ""
if (-not $InPath) {
    Write-Host "Note: Restart your terminal session or run `$env:Path = [System.Environment]::GetEnvironmentVariable('Path', 'User') to refresh PATH." -ForegroundColor Yellow
}
