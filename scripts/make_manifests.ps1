# PortPilot manifest 生成脚本(winget + scoop)
# 用法: powershell -File scripts\make_manifests.ps1 [-Version 0.2.0] [-Sha256 <zip哈希>]
#   不传 -Sha256 时自动计算 dist\PortPilot_v<Version>_win64.zip 的哈希。
# 前提: GitHub Release 已发布,zip 已作为资源上传(Release URL 是 manifest 的卡点,
#       见 docs/03 交接文档 §6:需用户在 GitHub 网页 Draft new release 并上传 zip)。
# 产物:
#   dist\manifests\winget\LJE.PortPilot\<Version>\*.yaml  (提交到 microsoft/winget-pkgs)
#   dist\manifests\PortPilot.json                          (提交到 scoop extras bucket)
param(
    [string]$Version = "0.2.0",
    [string]$Sha256 = ""
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$zip = Join-Path $root "dist\PortPilot_v${Version}_win64.zip"
if (-not (Test-Path $zip)) { throw "找不到 $zip —— 先跑 scripts\package.ps1 -Version $Version" }

if (-not $Sha256) {
    $Sha256 = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
}

# ⚠️ Release 发布后如果 tag/资源名与此 URL 不符,请同步修正
$releaseUrl = "https://github.com/lje970/PortPilot/releases/download/v${Version}/PortPilot_v${Version}_win64.zip"
$pkgId = "LJE.PortPilot"

# ---------- winget ----------
$wdir = Join-Path $root "dist\manifests\winget\$pkgId\$Version"
New-Item -ItemType Directory -Force -Path $wdir | Out-Null

$installer = @"
# Generated with scripts/make_manifests.ps1 - do not edit manually.
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.installer.1.6.0.schema.json
PackageIdentifier: $pkgId
PackageVersion: $Version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: PortPilot_v${Version}_win64\portpilot-app.exe
    PortableCommandAlias: portpilot
Installers:
  - Architecture: x64
    InstallerUrl: $releaseUrl
    InstallerSha256: $Sha256
ManifestType: installer
ManifestVersion: 1.6.0
"@

$locale = @"
# Generated with scripts/make_manifests.ps1 - do not edit manually.
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.defaultLocale.1.6.0.schema.json
PackageIdentifier: $pkgId
PackageVersion: $Version
PackageLocale: zh-CN
Publisher: lje970
PublisherUrl: https://github.com/lje970
PackageName: PortPilot
PackageUrl: https://github.com/lje970/PortPilot
License: Apache-2.0
LicenseUrl: https://github.com/lje970/PortPilot/blob/main/LICENSE
ShortDescription: 面向嵌入式开发者的开源串口调试工作台
Description: |-
  串口调试工作台:460800 满速零丢包(真机实测)、TCP/UDP 网络传输、
  日志回放、多标签会话、规则表自动应答、JustFloat/CSV 示波器、CRC 发送面板。
Tags:
  - serial
  - uart
  - embedded
  - debugging
  - oscilloscope
ManifestType: defaultLocale
ManifestVersion: 1.6.0
"@

$versionManifest = @"
# yaml-language-server: `$schema=https://aka.ms/winget-manifest.version.1.6.0.schema.json
PackageIdentifier: $pkgId
PackageVersion: $Version
ManifestType: version
ManifestVersion: 1.6.0
"@

Set-Content (Join-Path $wdir "$pkgId.installer.yaml") $installer -Encoding UTF8
Set-Content (Join-Path $wdir "$pkgId.locale.zh-CN.yaml") $locale -Encoding UTF8
Set-Content (Join-Path $wdir "$pkgId.yaml") $versionManifest -Encoding UTF8

# ---------- scoop ----------
$scoop = [ordered]@{
    version   = $Version
    url       = $releaseUrl
    hash      = $Sha256
    bin       = "PortPilot_v${Version}_win64\portpilot-app.exe"
    shortcuts = @("PortPilot_v${Version}_win64\portpilot-app.exe", "PortPilot")
    checkver  = @{ url = "https://github.com/lje970/PortPilot/releases/latest"; regex = "PortPilot_v([\d.]+)_win64\.zip" }
    autoupdate = @{ url = "https://github.com/lje970/PortPilot/releases/download/v{version}/PortPilot_v{version}_win64.zip" }
}
$scoopPath = Join-Path $root "dist\manifests\PortPilot.json"
$scoop | ConvertTo-Json -Depth 5 | Set-Content $scoopPath -Encoding UTF8

Write-Host "== winget manifest: $wdir =="
Write-Host "== scoop manifest: $scoopPath =="
Write-Host "SHA256: $Sha256"
Write-Host ""
Write-Host "后续步骤(需 GitHub Release 已发布):"
Write-Host "  winget: fork microsoft/winget-pkgs -> 按 dist\manifests\winget 结构放入 manifest/winget-pkgs -> 提 PR"
Write-Host "  scoop:  fork lukesampson/scoop-extras -> bucket/portpilot.json -> 提 PR"
