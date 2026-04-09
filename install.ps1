# merlint installer for Windows
# irm https://raw.githubusercontent.com/Link817290/Merlint/main/install.ps1 | iex

$ErrorActionPreference = "Stop"

Write-Host ""
Write-Host "  ==============================" -ForegroundColor Cyan
Write-Host "    merlint installer (Windows)" -ForegroundColor Cyan
Write-Host "  ==============================" -ForegroundColor Cyan
Write-Host ""

$installDir = "$env:USERPROFILE\.merlint\bin"
$exePath = "$installDir\merlint.exe"

# Create install directory
if (!(Test-Path $installDir)) {
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
}

# Download latest release
Write-Host "  [*] Downloading merlint..." -ForegroundColor Blue
$downloadUrl = $null
try {
    # Try all releases (sorted newest first)
    $releases = Invoke-RestMethod -Uri "https://api.github.com/repos/Link817290/Merlint/releases?per_page=5" -Headers @{ "User-Agent" = "merlint-installer" }
    foreach ($rel in $releases) {
        $asset = $rel.assets | Where-Object { $_.name -like "*windows*" } | Select-Object -First 1
        if ($asset) {
            $downloadUrl = $asset.browser_download_url
            Write-Host "  [*] Version: $($rel.tag_name)" -ForegroundColor Blue
            break
        }
    }
    if (!$downloadUrl) { throw "No Windows binary found" }
} catch {
    $downloadUrl = "https://github.com/Link817290/Merlint/releases/download/v0.1.2/merlint-windows-x64.exe"
}

Invoke-WebRequest -Uri $downloadUrl -OutFile $exePath -UseBasicParsing
Write-Host "  [+] Downloaded to $exePath" -ForegroundColor Green

# Add to PATH
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$installDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$installDir", "User")
    $env:Path = "$env:Path;$installDir"
    Write-Host "  [+] Added to PATH" -ForegroundColor Green
} else {
    Write-Host "  [+] Already in PATH" -ForegroundColor Green
}

# Verify
Write-Host ""
Write-Host "  ==========================================" -ForegroundColor Green
Write-Host "    Installation complete!" -ForegroundColor Green
Write-Host "  ==========================================" -ForegroundColor Green
Write-Host ""
Write-Host "  Restart your terminal, then run:" -ForegroundColor White
Write-Host "    merlint scan" -ForegroundColor Cyan
Write-Host ""
