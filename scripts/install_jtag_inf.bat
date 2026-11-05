@echo off
echo === Installing PortPilot JTAG INF ===
pnputil /add-driver "D:\serialport\scripts\portpilot_jtag.inf" /install
echo === Done! Press any key ===
pause > nul
