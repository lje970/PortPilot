# PortPilot: 重写 DeviceInterfaceGUIDs 到 MI_02 Device Parameters(nusb 实际读取位置)
# 背景:Zadig 驱动重装会重建设备键,抹掉手写的 GUID;Zadig 的 INF 只写软件键,
#       而 nusb 0.1.14 通过 CM_Open_DevNode_Key(CM_REGISTRY_HARDWARE) 读的是
#       Enum\...\Device Parameters 子键(实测 reg_diag 验证)。
# 运行:管理员权限
$ErrorActionPreference = 'Stop'

$instance = 'HKLM:\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001&MI_02\8&1BC25872&0&0002'
$guid = '{8F1D3A2B-7C4E-4F9A-9B2D-1E5F6A7B8C9D}'

Write-Host "== 1. DeviceParameters 写 DeviceInterfaceGUIDs =="
New-ItemProperty -Path "$instance\Device Parameters" -Name 'DeviceInterfaceGUIDs' `
    -PropertyType MultiString -Value $guid -Force | Out-Null
Write-Host "OK: $guid"

Write-Host "== 2. 实例根也写一份(兼容旧读取路径)="
New-ItemProperty -Path $instance -Name 'DeviceInterfaceGUIDs' `
    -PropertyType MultiString -Value $guid -Force | Out-Null
Write-Host "OK"

Write-Host "== 3. 重启父复合设备(winusb 重新注册接口)="
$parent = 'USB\VID_303A&PID_1001\E0:72:A1:D2:9B:88'
pnputil /restart-device "$parent"
Start-Sleep -Seconds 3

Write-Host "== 4. 验证 DeviceClasses 接口注册 =="
$dc = "HKLM:\SYSTEM\CurrentControlSet\Control\DeviceClasses\$guid"
if (Test-Path $dc) {
    Get-ChildItem $dc | ForEach-Object { Write-Host "  已注册接口: $($_.PSChildName)" }
} else {
    Write-Host "  DeviceClasses 未注册(接口还没生效,可能需再重启一次)"
}

Write-Host "== 完成 =="
