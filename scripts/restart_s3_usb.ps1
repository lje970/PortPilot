# PortPilot · 重启 ESP32-S3 USB 复合设备(使 WinUSB 重新注册设备接口)
# 运行:需管理员权限(提权运行,用户点 UAC "是")
$ErrorActionPreference = 'Stop'

$parent = 'USB\VID_303A&PID_1001\E0:72:A1:D2:9B:88'

Write-Host "== 禁用父复合设备 =="
Disable-PnpDevice -InstanceId $parent -Confirm:$false
Start-Sleep -Seconds 2

Write-Host "== 启用父复合设备 =="
Enable-PnpDevice -InstanceId $parent -Confirm:$false
Start-Sleep -Seconds 2

Write-Host "== 完成:WinUSB 应已用新 GUID 注册设备接口 =="
