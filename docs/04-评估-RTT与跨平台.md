# 评估:SEGGER RTT / VS Code 扩展 / macOS·Linux

> P2 清单中的三个"评估"项(规划书 §4.3),本文档给出结论与建议路径,评估日期 2026-11-04。

---

## 1. SEGGER RTT 支持

### 背景
RTT(Segger Real-Time Transfer)通过调试探针(J-Link/部分 CMSIS-DAP)从目标机 RAM 缓冲区读日志,
**不占用 UART 引脚、速率远高于串口**(实测可达 MB/s 级),是 Cortex-M 调试的"高级通道"。
开源竞品中仅 NinjaTerm 支持,是规划书确认的差异化空白。

### 两条技术路径

| 路径 | 说明 | 评估结论 |
|---|---|---|
| **probe-rs(Rust 原生)** | 开源调试探针框架,支持 J-Link/ST-Link/CMSIS-DAP 等大量探针,内置 RTT 读取(`probe-rs` crate 的 `rtt` 模块);与 PortPilot 的 Rust 栈**天然同源** | ✅ **推荐路径**。crate 活跃、纯 Rust 依赖、跨平台;RTT 需要目标机固件内嵌 RTT 控制块(常见于 embedded-rs 生态,`rtt-target` crate 一行接入) |
| SEGGER J-Link DLL | 闭源 DLL,Windows-only,功能最全 | ❌ 与开源承诺冲突;仅作为 probe-rs 之外的兼容层评估 |

### 接入设计草案(P3)
- 传输类型新增 `rtt`:连接参数 = 探针类型自动枚举 + 芯片名 + RTT 控制块地址(自动扫描 `.bss` 段中的 `SEGGER RTT` 签名,probe-rs 已有该能力)。
- I/O 线程改为:probe attach → 每 5ms 读 RTT 下行缓冲 → 进同一 SPSC 环;**规则表/示波器/日志/解码全部自动复用**——RTT 数据也是字节流,这正是 ITransport 抽象的红线收益。
- 前置条件:用户有调试探针(J-Link/ST-Link/CMSIS-DAP 均可,ST-Link 克隆约 ¥15)。
- 限制:RTT 需要目标机持续运行且未被复位;attach 模式(probe-rs `attach` 而非 `reset`)不干扰运行中的固件。

### 结论
**P3 实施,预估 1~2 周**(含探针枚举/RTT 扫描/错误路径)。差异化价值高(开源竞品仅 NinjaTerm 且不成熟),且 probe-rs 顺带打开未来"固件烧写/RTT/调试"一体化的门。

### 实测记录(2026-11-06,ESP32-S3 内建 USB-JTAG,VID 303A PID 1001)

| 环节 | 结果 | 证据 |
|---|---|---|
| WinUSB 驱动绑定 | ✅ 已攻破 | Zadig 装驱动 + `scripts/fix_guid_params.ps1` 重写 DeviceInterfaceGUIDs 到 `Device Parameters`(nusb 0.1.14 经 `CM_Open_DevNode_Key(CM_REGISTRY_HARDWARE)` 实际读的是 **Device Parameters 子键**,不是实例根;Zadig 重装驱动会抹掉手写 GUID,需在 Zadig 之后执行) |
| 探针枚举 | ✅ | probe-rs 0.28 内置;**0.32 起拆到 `probe-rs-espressif` crate**,启动时须调 `probe_rs_espressif::register_plugin()` |
| attach + 内存读写 | ✅ | 512KB DRAM 扫描 0.8s,128 块全读通 |
| RTT 控制块发现 | ✅ | `rtt_scan` 扫出 `SEGGER RTT` @ 0x3FC96AE4(与 ELF 符号一致) |
| RTT 数据读取 | ✅ | 读到 `"Hello from RTT #N"` 真实数据流 |
| **目标不停机实时流** | ❌ **Xtensa 阻塞** | 见下 |

**Xtensa 实时流阻塞点**(probe-rs XDM 已知限制,见上游 [issue #2001](https://github.com/probe-rs/probe-rs/issues/2001),0.28 与 0.32+probe-rs-espressif 实测相同):
1. attach 必停核:attach 即 halt,Xtensa `Core::run()` 返回 Ok 但核心不推进(status 报 Running、wrOff 冻结;疑似恢复路径破坏中断/寄存器状态);
2. CPU1 被 attach 塞软件断点(`Halted(Breakpoint(Software))`),run() 不清断点;
3. 复位陷阱:`reset_and_halt + run` 后,因调试模块仍使能,**ROM 检测到调试器就停等在下载模式**(PC=0x40041xxx,app 永不启动);烧录工具(esptool)的硬复位在调试器连接期间同样落此陷阱 → **工作流铁律:先烧录、后 attach;若顺序反了,拔插 USB 一次恢复**;
4. ESP32-S3 有 USB-CDC 串口兜底,RTT 实时流对 Xtensa 非刚需。

**结论修订**:RTT 首选目标平台改为 **ARM Cortex-M**(STM32H743 等,probe-rs ARM 路径成熟、resume 正常);Xtensa 平台提供"attach 快照读"模式(接上即读存量日志,`rtt_scan` 的起始追读已实现)。P3 app 集成按 ARM 为主设计 ITransport。

---

## 2. VS Code 扩展

### 现状
- 微软官方 Serial Monitor 扩展已停更;PlatformIO monitor 为 CLI;市场空缺确认(调研 §6.6)。
- PortPilot 已有两条可复用资产:**MCP server**(任何宿主可调)与 **core crate**(纯 Rust 库)。

### 三条路径对比

| 路径 | 说明 | 评估 |
|---|---|---|
| Webview 扩展包壳 | 扩展内嵌 Webview 加载现有前端 + Rust 扩展宿主(或经 MCP/子进程) | 功能最全但工程量大 |
| **轻量输出通道扩展** | 扩展只做"串口数据 → Output 通道",复用用户已开的 PortPilot GUI/MCP | 最小可用 |
| 纯 CLI 串口面板 | 类似 Serial Monitor 的 pickPort/波特率/HEX 视图,数据经扩展宿主直读 serialport(node 绑定) | 折中 |

### 结论
**P3 后段评估,暂缓**。理由:VS Code 扩展的维护面(发布/签名/版本矩阵)与当前单人节奏不匹配;
且 MCP server 已让 AI Agent 场景先行落地。触发条件:同学/用户出现明确 VS Code 内串口需求 ≥3 例。

---

## 3. macOS / Linux

### 可行性
- 核心层已是平台无关设计:serialport/TCP/UDP 三种传输均跨平台;**预计零核心代码改动**。
- Tauri 2 官方支持 macOS/Linux;需处理:签名公证(Apple Developer $99/年)、Linux 上 WebKitGTK 依赖、
  `.deb/.dmg` 打包、`/dev/tty.*` 枚举差异、无 DTR/RTS 差异(均支持)。
- 前端零改动(原生 JS)。

### 阻塞项(诚实清单)
1. **无 macOS/Linux 测试机**——CI 矩阵(GitHub Actions ubuntu/macos runner 免费)可部分缓解,但真机手感(权限弹窗、驱动)必须真机;
2. 用户群 90% 在 Windows(规划书 §2 决策依据),投入产出比当前不成立。

### 结论
**架构预留已到位,实施推迟**。触发条件:GitHub 上出现 macOS/Linux issue ≥5 例,或获得测试机。
届时的检查单已存于本文件 git 历史(P2 收官批)。

---

## 4. 决策记录

| 项 | 决策 | 触发条件 |
|---|---|---|
| RTT | **P3 实施**(probe-rs 路径) | P3 排期 |
| VS Code 扩展 | 暂缓 | 同类需求 ≥3 例 |
| macOS/Linux | 架构就绪,推迟 | 相关 issue ≥5 例或获得测试机 |
