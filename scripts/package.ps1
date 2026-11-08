# PortPilot 打包分发脚本
# 用法: powershell -File scripts\package.ps1 [-Version 0.1.0]
# 产物: dist\PortPilot_v<版本>_win64.zip(exe + WebView2Loader.dll + 使用说明)
param([string]$Version = "0.1.0")
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot

# 1) 构建 release(vcvars 环境与本仓库已知构建方式一致)
Write-Host "== 构建 portpilot-app(release) =="
cmd.exe /s /c "`"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat`" >nul 2>&1 && cd /d $root\src && set PATH=%USERPROFILE%\.cargo\bin;%PATH% && cargo build -p portpilot-app --release"
if ($LASTEXITCODE -ne 0) { throw "构建失败" }

# 2) 收集文件
$stage = Join-Path $root "dist\PortPilot_v${Version}_win64"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $stage | Out-Null
Copy-Item "$root\src\target\release\portpilot-app.exe" $stage
Copy-Item "$root\src\target\release\WebView2Loader.dll" $stage

# 3) 使用说明
$readme = @"
PortPilot v$Version 测试版(P3 · RTT + 录制重解析 + 频域)
==========================================

【运行要求】
- Windows 10(1903+)或 Windows 11,64 位
- WebView2 运行时:Win11 自带;Win10 大多自带。
  若打开后白屏/报错,先安装:https://developer.microsoft.com/microsoft-edge/webview2/

【运行方法】
1. 解压本文件夹到任意位置(保持 exe 与 dll 在同一目录)
2. 双击 portpilot-app.exe
3. 首次运行若出现"Windows 已保护你的电脑"(未签名测试版的正常提示):
   点"更多信息" -> "仍要运行"

【本版本已支持】
- 串口连接:预设波特率至 2M(含 256000 雷达常用档)+ 自定义;460800 满速零丢包
- RTT 调试探针(新):ESP32-S3 内建 USB-JTAG 直读日志,不占串口引脚(类型选 RTT)
- 网络传输:TCP 客户端 / TCP 服务端 / UDP
- 会话日志 pplog(新):带毫秒时间戳自描述存档,回放按真实节奏忠实重放
- 离线重解析(新):📐 解码面板选历史日志 → 🔄 一键用当前模板重新解码
- 示波器:时域 / 频域 FFT(新)双视图,JustFloat / CSV,峰值标记
- 断开体系(新):⏏ 断开常驻按钮,任意状态一键释放串口
- Lua 脚本(📜 脚本):自动应答/压测/协议仿真(tio 风格 API)
- 规则表自动应答:文本/HEX/正则匹配自动回发
- CRC 发送面板:7 种校验和 + 大小端
- 文本 / HEX 双模式;编码 UTF-8 / GBK;过滤高亮;虚拟滚动(百万行)
- AI/MCP ready:内置 MCP server,可被 Claude 等 AI 直接驱动

【已知限制(测试版)】
- TCP 服务端为单客户端语义
- RTT 当前支持 ESP32-S3 内建 USB-JTAG(需 WinUSB 驱动,详见仓库 docs)
- 未签名,SmartScreen 会提示

【反馈重点】
1. RTT:ESP32-S3 用户实测稳定性
2. 频域视图:传感器信号频谱是否符合预期
3. 离线重解析:历史日志 + 新模板的组合体验
4. 若崩溃或白屏:把 exe 同目录的 portpilot-panic.log 与 logs 文件夹发回来

【已知限制(测试版)】
- TCP 服务端为单客户端语义(断开后自动回监听)
- 未签名,SmartScreen 会提示

【反馈重点】(直接发群里,截图更好)
1. 网络传输:与你的 TCP/UDP 设备连通性如何?
2. 回放:选日志文件后能否正常回放、倍速是否生效?
3. 示波器/规则表/CRC 使用体验
4. 若崩溃或白屏:把 exe 同目录的 portpilot-panic.log 与 logs 文件夹发回来
"@
Set-Content -Path (Join-Path $stage "README.txt") -Value $readme -Encoding UTF8

# 4) 压缩
$zip = Join-Path $root "dist\PortPilot_v${Version}_win64.zip"
Compress-Archive -Path $stage -DestinationPath $zip -Force
Write-Host "== 打包完成: $zip =="
