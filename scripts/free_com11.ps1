# PortPilot: 重启 S3 USB 复合设备,强制释放 CDC(COM11) 上的僵尸句柄
# 场景:进程被强杀时持有的 USB-CDC 句柄可能残留,导致端口 PermissionError,
#       重启设备节点立即清空所有句柄(注册表配置如 DeviceInterfaceGUIDs 不受影响)。
$ErrorActionPreference = 'Stop'
$parent = 'USB\VID_303A&PID_1001\E0:72:A1:D2:9B:88'
Write-Host "== 重启 $parent =="
pnputil /restart-device "$parent"
Start-Sleep -Seconds 3
Write-Host "== 完成 =="
