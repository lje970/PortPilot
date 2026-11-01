# poc —— 四候选栈技术验证(M0)

统一验收清单与探针协议见 [probe_protocol.md](probe_protocol.md);测试执行与数据见 `../bench/`。

## 目录

| 目录 | 栈 | 状态 |
|---|---|---|
| `rust_core/` | **A** Tauri2+Rust 的核心层(serialport crate,console 形态) | ✅ 编译+实测通过(单 exe 527KB) |
| `tauri_app/` | **A** 的 Tauri GUI 壳 | ✅ **G1 通过(2026-11-01)**:MSVC 编译,同探针实测零丢包/延迟 3.43ms/核心进程 29MB/exe 8.7MB;踩坑记录见 ADR-001 §G1 执行记录 |
| `pyside6_app/` | **B** PySide6 + QSerialPort | ✅ 编译+实测通过(缓解后) |
| `electron_app/` | **C** Electron + node-serialport | ✅ 编译+实测通过 |
| `wpf_app/` | **D** C#/WPF + System.IO.Ports | ✅ 编译+实测通过(两轮缓解后仍丢包) |

## 统一测试场景

```
bench 纯发生器 --COM2--> [Eltima 虚拟对 COM1<->COM2,链路基线零丢包] --COM1--> 被测 POC
921600(标称) · 30s · 256B 序号块 · 实际链路吞吐 ~9-11 Mbps(虚拟对上限 ~10.7 Mbps)
```

执行:`python bench/run_poc_test.py --tag <栈> --cmd "<运行命令>" --probe-out <jsonl> --force --seconds 30`

## 最终实测结果(2026-11-01,详见 bench/results/)

| 栈 | 管线丢包 | 延迟p95(ms) | UI fps | RSS 均值/峰值 | 便携产物 | 结论 |
|---|---|---|---|---|---|---|
| **A Rust core** | **~0**(-0.09%) | **0.23** | 25.9 | **13.3 / 14.0 MB** | **527KB 单 exe** | 🏆 全维度最优 |
| **B PySide6** | 0.17%(裸跑 1.55%) | 0.46 | 18.9 | 76.6 / 78.8 MB | 未打包(PyInstaller 已知体积大+杀软误报) | 良好,丢包需轮询兜底缓解 |
| **C Electron** | 0.097% | 1.09 | 25.9 | **429.9 / 468.7 MB** | dist 318MB | 功能达标,内存/体积不合格 |
| **D WPF** | **8.8%**(裸跑 12.3%) | 0.56 | 16.8 | 134.0 / 142.5 MB | 153.6MB 自包含单 exe | ❌ System.IO.Ports 持续高流量丢数据,两轮缓解未根治 |

## 过程中学到的工程知识(写入 ADR-001)

1. **QSerialPort readyRead 在 ~10Mbps 持续流下会丢数据**;加 2ms 轮询兜底后达标 —— 印证调研中"Qt 高吞吐需缓冲设计"的口碑。
2. **System.IO.Ports 丢包严重**(12.3%),ArrayPool 复用 + 256KB 驱动缓冲后仍 8.8% —— 与调研"Windows 最稳"的印象相反,持续高流量场景是它的盲区。
3. **Electron 管线本身扛得住**(IPC 批处理正确实现时),但 430MB 内存坐实了调研结论。
4. Rust 核心层零丢包、亚毫秒延迟、14MB 内存,且单文件 527KB。
5. 收尾在途残余:POC 排水窗口统一 +5s 后,各栈残余 ≤0.2%(负值=吸收了上轮链路缓冲残留)。

## 已知限制(诚实记录)

- 虚拟对测试的是**应用管线**,不代表真实 UART 驱动行为;CH340(COM12)端口已空闲但**未接 TX-RX 短接头**(2026-11-01 探测 0/8),真实硬件回环列入 M1 门(G2/G3)。
- Tauri GUI 的 emit 路径存在堆分配竞争(开启事件时丢包 5.6%,NO_EMIT 对照零丢包)——**产品化必须给 I/O 线程做免分配环形缓冲 + 线程优先级保护**(ADR-001 G1 执行记录第 3 条);POC 层已用 NO_EMIT 开关隔离验证。
- 内存口径:树合计 RSS-sum 会重复计入 WebView2 共享内存;对比请看核心进程 RSS(rust-core 14MB / tauri 主进程 29MB)。
- 冷启动未做正式测量(非决定性指标):观察值 Rust ≈即时、WPF/Electron ≈1s、PySide6 ≈1.5s。
- Electron POC 用纯 JS(产品化转 TS);PySide6 未做打包(打包问题引用调研结论)。
