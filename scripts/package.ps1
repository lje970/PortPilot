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
PortPilot v$Version 测试版(M2 · 网络传输 + 日志回放)
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
- 串口连接:预设波特率至 460800 + 自定义;460800 满速零丢包(真机实测)
- 网络传输(新):TCP 客户端 / TCP 服务端 / UDP,连接栏切换传输类型
- 日志回放(新):⏵ 回放 → 选 logs\*.bin → 倍速回放到当前标签
- 多标签会话:每个标签独立连接/编码/过滤/暂停
- 规则表自动应答:文本/HEX/正则匹配关键词自动回发(命中冷却 300ms)
- 示波器:JustFloat / CSV 协议,canvas 实时波形,自适应 Y 轴
- CRC 发送面板:7 种校验和 + 大小端,输入实时预览、发送自动追加
- 文本 / HEX 双模式收发;编码 UTF-8 / GBK 切换
- 过滤高亮(包含/排除/正则)、时间戳、暂停、清屏
- 虚拟滚动(百万行);发送历史 + 8 个快捷按钮;DTR/RTS 线控
- 会话原始字节流自动落盘(exe 同目录 logs\PortPilot_*.bin)

【已知限制(测试版)】
- TCP 服务端为单客户端语义(断开后自动回监听)
- 未签名,SmartScreen 会提示

【反馈重点】(直接发群里,截图更好)
1. 网络传输:与你的 TCP/UDP 设备连通性如何?
2. 回放:选日志文件后能否正常回放、倍速是否生效?
3. 示波器/规则表/CRC 使用体验
4. 若崩溃或白屏:把 exe 同目录的 portpilot-panic.log 与 logs 文件夹发回来
"@
Set-Content -Path (Join-Path $stage "使用说明.txt") -Value $readme -Encoding UTF8

# 4) 压缩
$zip = Join-Path $root "dist\PortPilot_v${Version}_win64.zip"
Compress-Archive -Path $stage -DestinationPath $zip -Force
Write-Host "== 打包完成: $zip =="
