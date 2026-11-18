# Installs juicetop on Windows and starts the background collector. Untested. Run it again to
# update; it keeps your settings, data and the kind of collector you chose.
#
#   irm https://raw.githubusercontent.com/CookedApps/juicetop/main/install.ps1 | iex
#
# From an administrator terminal the collector runs as SYSTEM from boot. Otherwise it runs as you
# from logon. Environment:
#   $env:JUICETOP_VERSION = "v0.2.0"   install a specific release instead of the latest
#   $env:JUICETOP_SERVICE = "none"     only install the binary

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$repo = "CookedApps/juicetop"
$version = if ($env:JUICETOP_VERSION) { $env:JUICETOP_VERSION } else { "latest" }
$service = if ($env:JUICETOP_SERVICE) { $env:JUICETOP_SERVICE } else { "auto" }

function Say($msg) { Write-Host "==> $msg" -ForegroundColor Yellow }

if (-not [Environment]::Is64BitOperatingSystem) { throw "juicetop needs 64-bit Windows" }
$target = "x86_64-pc-windows-msvc"
$archive = "juicetop-$target.zip"
$base = if ($version -eq "latest") {
    "https://github.com/$repo/releases/latest/download"
} else {
    "https://github.com/$repo/releases/download/$version"
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("juicetop-" + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Say "downloading $archive ($version)"
    Invoke-WebRequest "$base/$archive" -OutFile "$tmp\$archive" -UseBasicParsing
    Invoke-WebRequest "$base/$archive.sha256" -OutFile "$tmp\$archive.sha256" -UseBasicParsing
    $expected = ((Get-Content "$tmp\$archive.sha256" -Raw).Trim() -split "\s+")[0].ToLower()
    $actual = (Get-FileHash "$tmp\$archive" -Algorithm SHA256).Hash.ToLower()
    if ($expected -ne $actual) { throw "checksum mismatch" }
    Expand-Archive "$tmp\$archive" -DestinationPath "$tmp\out" -Force

    $dir = Join-Path $env:LOCALAPPDATA "Programs\juicetop"
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $exe = "$dir\juicetop.exe"
    $new = ((& "$tmp\out\juicetop.exe" --version) -split " ")[1]
    if (Test-Path $exe) {
        $old = ((& $exe --version) -split " ")[1]
        Say "updating juicetop $old to $new"
        # A running program cannot be overwritten on Windows, but it can be renamed.
        Remove-Item "$dir\juicetop.old.exe" -Force -ErrorAction SilentlyContinue
        Move-Item $exe "$dir\juicetop.old.exe" -Force
    } else {
        Say "installing juicetop $new"
    }
    Copy-Item "$tmp\out\juicetop.exe" $exe -Force
    Say "installed $exe"

    $path = [Environment]::GetEnvironmentVariable("Path", "User")
    if (-not (($path -split ";") -contains $dir)) {
        [Environment]::SetEnvironmentVariable("Path", "$path;$dir", "User")
        $env:Path = "$env:Path;$dir"
        Say "added $dir to your PATH (open a new terminal to use it)"
    }

    $admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    # An update keeps the collector the user picked last time.
    if ($service -eq "auto") {
        schtasks /Query /TN juicetop *> $null
        $hasSystem = $LASTEXITCODE -eq 0
        schtasks /Query /TN juicetop-user *> $null
        $hasUser = $LASTEXITCODE -eq 0
        if ($hasSystem -and -not $admin) {
            throw "the collector runs as SYSTEM; run this again in an administrator terminal to update it"
        }
        if ($hasUser -and -not $hasSystem) { $service = "user" }
    }
    $jtArgs = switch ($service) {
        "none" { $null }
        "user" { @("service", "install", "--user") }
        "system" { @("service", "install") }
        "auto" { if ($admin) { @("service", "install") } else { @("service", "install", "--user") } }
        default { throw "JUICETOP_SERVICE must be auto, system, user or none" }
    }
    if ($null -eq $jtArgs) {
        Say "no collector installed; start one with: juicetop service install"
    } else {
        & "$dir\juicetop.exe" @jtArgs
        if ($LASTEXITCODE -ne 0) { throw "setting up the collector failed" }
    }
    Say "done. Open the dashboard with: juicetop (run this again to update)"
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
