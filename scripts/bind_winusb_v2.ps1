# PortPilot: Bind WinUSB device interface GUID to ESP32-S3 JTAG interface (MI_02)
# Requires admin privileges
$ErrorActionPreference = 'Stop'

$instance = 'USB\VID_303A&PID_1001&MI_02\8&1BC25872&0&0002'
$guid = '{8F1D3A2B-7C4E-4F9A-9B2D-1E5F6A7B8C9D}'

Write-Host "=== 1. Write DeviceInterfaceGUIDs (instance key root) ==="
$instance_key = "HKLM:\SYSTEM\CurrentControlSet\Enum\$instance"
New-ItemProperty -Path $instance_key -Name 'DeviceInterfaceGUIDs' `
    -PropertyType MultiString -Value $guid -Force | Out-Null
Write-Host "OK"

Write-Host "=== 2. Write DeviceInterfaceGUIDs (DeviceParameters subkey) ==="
$dp = "$instance_key\DeviceParameters"
if (-not (Test-Path $dp)) { New-Item -Path $dp -Force | Out-Null }
New-ItemProperty -Path $dp -Name 'DeviceInterfaceGUIDs' `
    -PropertyType MultiString -Value $guid -Force | Out-Null
Write-Host "OK"

Write-Host "=== 3. Disable/Enable MI_02 ==="
Disable-PnpDevice -InstanceId $instance -Confirm:$false -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
Enable-PnpDevice -InstanceId $instance -Confirm:$false -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

Write-Host "=== 4. Disable/Enable parent composite device ==="
$parent = 'USB\VID_303A&PID_1001\E0:72:A1:D2:9B:88'
Disable-PnpDevice -InstanceId $parent -Confirm:$false -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2
Enable-PnpDevice -InstanceId $parent -Confirm:$false -ErrorAction SilentlyContinue
Start-Sleep -Seconds 2

Write-Host "=== Verify: DeviceClasses registration ==="
$class_guid_path = "HKLM:\SYSTEM\CurrentControlSet\Control\DeviceClasses\{$guid}"
if (Test-Path $class_guid_path) {
    Write-Host "DeviceClasses registered OK"
    Get-ChildItem $class_guid_path | ForEach-Object { Write-Host "  Interface: $($_.PSChildName)" }
} else {
    Write-Host "DeviceClasses NOT registered (may need additional restart)"
}

Write-Host "=== All done ==="
