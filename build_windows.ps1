# Baut Konvex Compute unter Windows (Rust + Tauri 2) und installiert es auf Wunsch
# für den aktuellen Benutzer, ohne Administratorrechte.
#
#   powershell -ExecutionPolicy Bypass -File .\build_windows.ps1            nur bauen
#   powershell -ExecutionPolicy Bypass -File .\build_windows.ps1 -Install   bauen und installieren

param([switch]$Install)

$ErrorActionPreference = "Stop"

$Root = $PSScriptRoot
$TauriDir = Join-Path $Root "src-tauri"
$AppName = "Konvex Compute"
$BuiltExe = Join-Path $TauriDir "target\release\konvex-compute.exe"

Write-Host "==> Baue Release-Binary (Rust + Tauri 2, locked)..."
Push-Location $TauriDir
try {
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) {
        throw "cargo build ist fehlgeschlagen (Exit-Code $LASTEXITCODE)."
    }
} finally {
    Pop-Location
}

if (-not $Install) {
    Write-Host "Gebaut: $BuiltExe"
    exit 0
}

if (Get-Process -Name $AppName, "konvex-compute" -ErrorAction SilentlyContinue) {
    throw "$AppName läuft noch und muss vor der Installation beendet werden."
}

$DestDir = Join-Path $env:LOCALAPPDATA "Programs\$AppName"
$DestExe = Join-Path $DestDir "$AppName.exe"

Write-Host "==> Kopiere nach $DestDir ..."
New-Item -ItemType Directory -Force -Path $DestDir | Out-Null
Copy-Item -LiteralPath $BuiltExe -Destination $DestExe -Force

Write-Host "==> Lege Startmenü-Eintrag an..."
$StartMenuDir = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs"
$ShortcutPath = Join-Path $StartMenuDir "$AppName.lnk"
$Shell = New-Object -ComObject WScript.Shell
$Shortcut = $Shell.CreateShortcut($ShortcutPath)
$Shortcut.TargetPath = $DestExe
$Shortcut.WorkingDirectory = $DestDir
$Shortcut.IconLocation = "$DestExe,0"
$Shortcut.Save()

Write-Host "Installiert: $DestExe"
Write-Host "Startmenü: $ShortcutPath"
