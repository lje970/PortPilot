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
PortPilot v$Version 测试版(M1 · MVP 骨干)
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
- 串口连接:预设波特率至 460800 + 自定义;921600 可试(探索档)
- 文本 / HEX 双模式收发;发送新行 CR/LF/CRLF 可选
- 时间戳、暂停、清屏、自动滚动;行数与 RX/TX 实时统计
- 发送历史(输入框按上下键);8 个快捷发送按钮(双击编辑,本地保存)
- DTR/RTS 线控(一键复位板卡)
- 会话原始字节流自动落盘(exe 同目录 logs\PortPilot_*.bin)

【已知限制(测试版,下版解决)】
- 终端缓冲上限 4000 行(虚拟滚动开发中)
- 编码固定 UTF-8(GBK 切换开发中)
- 暂无连接档案保存/切换

【反馈重点】(直接发群里,截图更好)
1. 能否正常打开?系统是 Win10 还是 Win11?有没有提示装 WebView2?
2. SmartScreen 拦截长什么样(截图)?
3. 有串口设备的话:收发是否正常?有无乱码?高波特率下卡不卡、丢不丢数据?
4. UI 任何别扭之处(布局/字体/颜色/交互)
5. 若崩溃或白屏:把 exe 同目录的 portpilot-panic.log 与 logs 文件夹发回来
"@
Set-Content -Path (Join-Path $stage "使用说明.txt") -Value $readme -Encoding UTF8

# 4) 压缩
$zip = Join-Path $root "dist\PortPilot_v${Version}_win64.zip"
Compress-Archive -Path $stage -DestinationPath $zip -Force
Write-Host "== 打包完成: $zip =="
