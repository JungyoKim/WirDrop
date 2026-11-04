<#
.SYNOPSIS
Builds WirDrop and lays out a folder it runs from.

.DESCRIPTION
Puts WirDrop.exe, wirdrop_shell.dll, usbfwd.exe, appleid.exe and appleid-anisette.exe (the Apple
ID sign-in, the second one 32-bit) (cargo build --release), and wintun.dll in
-Out, with the Explorer menu's package: WirDrop.ContextMenu.msix, and its manifest in package\
(which WirDrop reads to tell when the registered one is out of date). Asks a WirDrop running from
-Out to quit first, and stops the COM surrogate holding its menu DLL.

The package isn't signed: its Publisher marks it as unsigned, and WirDrop's setup registers it
with -AllowUnsigned, as an administrator.

.EXAMPLE
.\stage.ps1 -Out D:\WirDrop -Wintun C:\wintun\bin\amd64\wintun.dll
#>
param(
    [Parameter(Mandatory)] [string] $Out,
    # Folder with another usbfwd.exe to use instead of the one built from radio/. Optional.
    [string] $Usbfwd,
    # wintun.dll (amd64) from wintun.net.
    [Parameter(Mandatory)] [string] $Wintun,
    [switch] $SkipBuild
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path "$PSScriptRoot\.."
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root 'target' }

if (-not $SkipBuild) {
    Push-Location $root
    try {
        cargo build --release -p wirdrop-app -p wirdrop-shell -p wirdrop-appleid
        if ($LASTEXITCODE) { throw "cargo build failed" }
        cargo build --release -p wirdrop-radio --bin usbfwd
        if ($LASTEXITCODE) { throw "cargo build failed" }
        # AOSKit, which it loads, is 32-bit.
        cargo build --release -p wirdrop-appleid-anisette --target i686-pc-windows-msvc
        if ($LASTEXITCODE) { throw "cargo build failed (rustup target add i686-pc-windows-msvc?)" }
    } finally { Pop-Location }
}

New-Item -ItemType Directory -Force $Out | Out-Null
$Out = (Resolve-Path $Out).Path

$release = Join-Path $target 'release'

# A running WirDrop holds its exe. The engine runs elevated, so ask the tray app to quit, and only
# when the exe changed and that WirDrop runs from here: --exit reaches any running WirDrop, including
# an installed one this script has nothing to do with.
$exeChanged = -not (Test-Path "$Out\WirDrop.exe") -or
    (Get-FileHash "$release\WirDrop.exe").Hash -ne (Get-FileHash "$Out\WirDrop.exe").Hash
$runsHere = Get-Process WirDrop -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Out\*" }
if ($exeChanged -and $runsHere) {
    & "$Out\WirDrop.exe" --exit
    for ($i = 0; $i -lt 20 -and (Get-Process WirDrop -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Out\*" }); $i++) {
        Start-Sleep -Milliseconds 250
    }
}
if ($exeChanged) {
    Get-Process WirDrop -ErrorAction SilentlyContinue | Where-Object { $_.Path -like "$Out\*" } | Stop-Process -Force -ErrorAction SilentlyContinue
}
Get-Process dllhost -ErrorAction SilentlyContinue |
    Where-Object { try { $_.Modules.FileName -like "$Out\wirdrop_shell.dll" } catch { $false } } |
    Stop-Process -Force
Start-Sleep -Milliseconds 300

# Copies a file unless the same one is already there. A file in use (running, or an icon source
# Explorer holds) can't be overwritten but can be renamed: move it aside and copy beside it.
function Copy-Changed([string] $From, [string] $To) {
    $dest = if (Test-Path $To -PathType Container) { Join-Path $To (Split-Path $From -Leaf) } else { $To }
    if ((Test-Path $dest) -and (Get-FileHash $From).Hash -eq (Get-FileHash $dest).Hash) { return }
    try {
        Copy-Item $From $dest -Force -ErrorAction Stop
    } catch {
        Rename-Item $dest ("{0}.old-{1}" -f (Split-Path $dest -Leaf), [DateTime]::Now.Ticks)
        Copy-Item $From $dest -Force
    }
}

# Files moved aside by an earlier run, once nothing holds them.
Get-ChildItem $Out -Filter '*.old-*' -ErrorAction SilentlyContinue | Remove-Item -Force -ErrorAction SilentlyContinue

Copy-Changed "$release\WirDrop.exe" $Out
Copy-Changed "$release\wirdrop_shell.dll" $Out
Copy-Changed (Join-Path $(if ($Usbfwd) { $Usbfwd } else { $release }) 'usbfwd.exe') $Out
Copy-Changed $Wintun (Join-Path $Out 'wintun.dll')
# Licenses: ours, and the notices the BSD-3-Clause rtw88 port and Realtek's firmware ask for.
Copy-Changed (Join-Path $root 'LICENSE') (Join-Path $Out 'LICENSE.txt')
Copy-Changed (Join-Path $root 'crates\radio\NOTICE.md') (Join-Path $Out 'NOTICE-radio.md')
Copy-Changed (Join-Path $root 'crates\radio\firmware\LICENCE.rtlwifi_firmware.txt') $Out
Copy-Changed "$release\appleid.exe" $Out
Copy-Changed (Join-Path $target 'i686-pc-windows-msvc\release\appleid-anisette.exe') $Out

# The Explorer menu package.
New-Item -ItemType Directory -Force "$Out\icons" | Out-Null
Copy-Changed (Join-Path $root 'crates\app\res\wirdrop.ico') "$Out\icons"
Copy-Changed (Join-Path $root 'crates\app\res\phone.ico') "$Out\icons"
New-Item -ItemType Directory -Force "$Out\package\Assets" | Out-Null
$menuPackage = Join-Path $PSScriptRoot 'explorer-menu'
Copy-Item "$menuPackage\AppxManifest.xml" "$Out\package"
Copy-Item "$menuPackage\Assets\*.png" "$Out\package\Assets"
$makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" |
    Sort-Object { [version]$_.Directory.Parent.Name } | Select-Object -Last 1
if (-not $makeappx) { throw "makeappx.exe not found: install the Windows SDK" }
$pkg = Join-Path ([IO.Path]::GetTempPath()) "wirdrop-msix-$PID"
Remove-Item $pkg -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force "$pkg\Assets" | Out-Null
Copy-Item "$menuPackage\AppxManifest.xml" $pkg
Copy-Item "$menuPackage\Assets\*.png" "$pkg\Assets"
$msix = Join-Path $Out 'WirDrop.ContextMenu.msix'
& $makeappx.FullName pack /d $pkg /p $msix /o /nv | Out-Null
if ($LASTEXITCODE) { throw "makeappx failed" }
Remove-Item $pkg -Recurse -Force

Get-ChildItem $Out | Select-Object Name, Length

