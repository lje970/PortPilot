# AGENTS.md — PortPilot 开发者/AI 工作指南

> 面向人类贡献者与 AI 编码代理的仓库导航。读完本文件即可安全修改代码。

## 项目一句话

Windows 优先的开源串口调试工作台:终端 + 示波器(时域/频域)+ 协议解码 + Lua 自动化 + AI/MCP。
**Tauri 2 + Rust** 核心,vanilla JS 前端。License:Apache-2.0。

## 目录结构

```
docs/            01 调研 / 02 规划 / 03 交接文档(权威,先读) / 04 RTT与跨平台评估 / decisions/ADR-001
src/             工作区(workspace)
  crates/portpilot-core    平台无关核心:传输抽象/SPSC环/会话/解码器/脚本/pplog/FFT/统计
  crates/portpilot-mcp     MCP server(stdio JSON-RPC,7 工具,AI 驱动入口)
  crates/portpilot-rtt     RTT 传输 + 诊断工具链(probe-rs 0.32 + probe-rs-espressif)
  app/src-tauri            Tauri 后端(多标签会话/泵线程/命令)
  app/ui                   前端(index.html + app.js + style.css,无框架)
bench/           基准脚本(回环丢包/矩阵);mcu_firmware/ ESP32 测试固件
scripts/         打包/图标/驱动工具
dist/            发布产物(v0.5.0 起)
poc/             M0 四栈 POC(历史,勿改)
```

## 构建与测试

```bash
# 构建(必须 MSVC;Git Bash 下直接 cargo 可用,linker 已在 .cargo/config 显式)
cd src && cargo build -p portpilot-app --release

# 全部单测(core 45 + 集成 2)
cargo test -p portpilot-core

# 基准回归门(阈值 6s,超时即回归)
cargo test -p portpilot-core --test script_rate

# 打包(P3 说明清单已内置)
powershell -File scripts/package.ps1 -Version 0.5.0
```

## 必须遵守的红线(踩过的坑,详见 docs/03 §7)

1. **I/O 热路径零堆分配、零 emit**——WebView 事件 IPC 曾致 5.6% 丢包。
2. **mutex 内禁止 sleep**——坑 22:slot 锁饥饿让 Lua 节拍慢 24 倍。锁内只做拷贝/算术。
3. **空转轮询必须周期性让出锁/_cpu**(坑 22 配套)。
4. **串口读超时 = Ok(0)**(坑 17);Windows TCP 读超时也是 Ok(0) 非 Err。
5. **RTT 读取路径严禁 halt()/reset()**(Xtensa resume 缺陷);只走 MemoryInterface。
6. **app.js 新增 `$('id')` 绑定必须同步确认 index.html 有该元素**(坑 18:scriptBtn 缺失致前端加载死)。
7. **disconnect 必须同时停泵线程标志与 handle.stop()**(坑 19:漏调致端口永久占用)。
8. **[profile.release] 放 workspace root**,放 app 包内会被忽略(坑 23,strip 失效 exe 47MB)。
9. **断线重连风暴排查**:先确认数据源是否静默(坑 17)。
10. **SerialWeb 是 AGPL-3.0**:借鉴思路可以,一行代码/CSS/图标都不能抄。

## 架构 30 秒

```
UI ──(Tauri emit: rx/scope/decoded)── 泵线程(33ms 批)── SPSC 环 ←── 会话 I/O 线程 ← Transport trait
```
- Transport trait 实现者:Serial / TcpClient / TcpServer / Udp / Loop / Rtt
- 泵线程 = 行组装 + HEX + 规则 + 示波器抽头 + 解码抽头 + pplog 落盘
- StreamAnalyzer(main.rs):同一分析逻辑被实时与回放共用
- pplog v1:会话日志自描述格式(`PPLG` + meta + `[ms][dir][len][bytes]`)

## 测试约定

- core 层一切可单测;新增算法(如 FFT/解码/pplog)必须带单测
- 涉及锁/线程行为的修复必须留基准或回归测试(例:tests/script_rate.rs)
- 硬件相关的用 `--kind loop` 冒烟替代

## 与 AI/MCP

- `crates/portpilot-mcp`:stdio MCP server,AI 可直接连接/发送/等待/统计
- 本文件与 `llms.txt` 是"AI 能读懂仓库"的入口;MCP 是"AI 能驱动工具"
