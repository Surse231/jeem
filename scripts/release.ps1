# Builds the signed installer and publishes it to the public Surse231/jeem repo.
# Usage: powershell -File scripts\release.ps1 -Notes "What changed"
param([string]$Notes = "")

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$keyFile = Join-Path $env:USERPROFILE ".tauri\jeem-updater.key"
$env:TAURI_SIGNING_PRIVATE_KEY = (Get-Content $keyFile -Raw).Trim()
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = (Get-Content (Join-Path $env:USERPROFILE ".tauri\jeem-updater.password") -Raw).Trim()

# Google OAuth client (Desktop app) baked into the build; the file lives outside the repo.
$googleFile = Join-Path $env:USERPROFILE ".tauri\jeem-google-client.json"
if (Test-Path $googleFile) {
  $g = Get-Content $googleFile -Raw | ConvertFrom-Json
  $env:JEEM_GOOGLE_CLIENT_ID = $g.client_id
  $env:JEEM_GOOGLE_CLIENT_SECRET = $g.client_secret
} else {
  Write-Host "WARNING: $googleFile not found - Google sign-in will NOT be configured in this build"
}

$conf = Get-Content "src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json
$version = $conf.version
$tag = "v$version"

npm run build
if ($LASTEXITCODE -ne 0) { throw "Build failed" }

$dir = "src-tauri\target\release\bundle\nsis"
$exe = Get-ChildItem $dir -Filter "*_${version}_x64-setup.exe" | Select-Object -First 1
$sig = Get-Content "$($exe.FullName).sig" -Raw
$url = "https://github.com/Surse231/jeem/releases/download/$tag/$($exe.Name)"

$latest = [ordered]@{
  version  = $version
  notes    = $Notes
  pub_date = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
  platforms = [ordered]@{
    "windows-x86_64" = [ordered]@{ signature = $sig.Trim(); url = $url }
  }
}
$latestPath = Join-Path $dir "latest.json"
$json = $latest | ConvertTo-Json -Depth 5
[System.IO.File]::WriteAllText((Join-Path $root $latestPath), $json, (New-Object System.Text.UTF8Encoding($false)))

gh release create $tag $exe.FullName $latestPath --repo Surse231/jeem --title "Jeem $tag" --notes $Notes
