@echo off
echo === Writing DeviceInterfaceGUIDs ===
reg add "HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001&MI_02\8&1BC25872&0&0002" /v DeviceInterfaceGUIDs /t REG_MULTI_SZ /d "{8F1D3A2B-7C4E-4F9A-9B2D-1E5F6A7B8C9D}" /f
echo === Writing DeviceParameters ===
reg add "HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001&MI_02\8&1BC25872&0&0002\DeviceParameters" /v DeviceInterfaceGUIDs /t REG_MULTI_SZ /d "{8F1D3A2B-7C4E-4F9A-9B2D-1E5F6A7B8C9D}" /f
echo === Restarting device ===
pnputil /restart-device "USB\VID_303A&PID_1001&MI_02\8&1BC25872&0&0002"
echo === Verifying ===
reg query "HKLM\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001&MI_02\8&1BC25872&0&0002" /v DeviceInterfaceGUIDs
echo === Done! Press any key ===
pause > nul
