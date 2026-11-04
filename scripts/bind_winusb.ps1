# PortPilot · 给 ESP32-S3 JTAG 接口(MI_02)补写 DeviceInterfaceGUIDs 并重启设备
# 背景:winusb.inf 已绑定为功能驱动,但 MS OS 描述符未生成设备接口 GUID,
#       nusb/probe-rs 需要 DeviceInterfaceGUIDs 才能打开设备(实测报错实锤)。
# 运行:需管理员权限(写 HKLM + 重启设备)
$ErrorActionPreference = 'Stop'

$instance = 'USB\VID_303A&PID_1001&MI_02\8&1BC25872&0&0002'
$guid = '{8F1D3A2B-7C4E-4F9A-9B2D-1E5F6A7B8C9D}'  # PortPilot RTT 专用接口 GUID

Write-Host "== 写 DeviceInterfaceGUIDs(实例键根)=="
# GUID 写在实例键根(nusb registry_key() 直接读这里,不走 DeviceParameters 子键)
$key = "HKLM:\SYSTEM\CurrentControlSet\Enum\$instance"
New-ItemProperty -Path $key -Name 'DeviceInterfaceGUIDs' `
    -PropertyType MultiString -Value $guid -Force | Out-Null
Write-Host "已写入 $guid"

Write-Host "== 重启设备(重新注册接口)=="
pnputil /restart-device "$instance"
Start-Sleep -Seconds 2

Write-Host "== 完成:接口应已带 GUID 重新注册 =="
