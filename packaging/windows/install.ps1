# Installs glenley-agent for the current user (no admin rights needed) and runs setup.
#   powershell -ExecutionPolicy Bypass -File install.ps1 -Server https://YOUR-CARSON-SERVER
param([Parameter(Mandatory = $true)][string]$Server)
$ErrorActionPreference = "Stop"
$dir = Join-Path $env:LOCALAPPDATA "Programs\glenley"
New-Item -ItemType Directory -Force -Path $dir | Out-Null
Copy-Item -Force (Join-Path $PSScriptRoot "glenley-agent.exe") $dir
# Make `glenley-agent` available in new terminals.
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not ($userPath -split ";" | Where-Object { $_ -eq $dir })) {
  [Environment]::SetEnvironmentVariable("Path", "$userPath;$dir", "User")
}
& (Join-Path $dir "glenley-agent.exe") setup --server $Server
