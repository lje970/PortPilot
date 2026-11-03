# PortPilot

> 面向嵌入式开发者的现代化开源串口调试工作台 —— 把"终端收发、实时示波器、协议帧解码"合为一体。
>
> **[English](README.en.md)**

**状态:M1 开发中(MVP 0.1.0)** · 技术栈:**Tauri 2 + Rust** · License:**Apache-2.0** · 平台:Windows 10/11 优先(架构预留跨平台)

## 为什么做这个

2026 年的串口调试工具市场:闭源标杆(SSCOM/串口猎人)停更、头部开源(Serial Studio 4.x)转向 Pro 付费分层、终端套件(Tabby/WindTerm)串口能力浅、收发与示波器割裂(VOFA+/SerialPlot)。**"现代化 UI + 高吞吐稳定 + 无门槛自动化 + 三合一 + 全功能免费开源"没有占位者** —— PortPilot 补这个位。

承诺:**核心全功能免费开源,不搞 Pro 锁功能。**

## 实测指标(M0~M1 技术验证,全部可复跑)

| 指标 | 数值 | 验证方式 |
|---|---|---|
| 460800 bps 满速持续流 | **零丢包**(5631 块/30s,真实 MCU 灌流) | ESP32-S3 灌流 + 序号校验([bench](bench/)) |
| 真机回环矩阵 | 115200~921600 四档全零丢包;460800 达线速 99.1% | CH340 硬件回环 |
| 管线延迟(到达→上屏)p95 | **0.23 ms**(console)/ 3.43 ms(GUI) | 统一探针协议 |
| 内存 | 核心进程 **14~29 MB**(对比 Electron 同场景 430 MB) | 四栈 POC 同场景对比 |
| 便携产物 | **527 KB 单 exe**(core)/ 8.7MB(GUI 壳) | cargo release |

> 对比数据来自四候选栈(Tauri2+Rust / PySide6 / Electron / WPF)同验收清单实测,WPF 因 System.IO.Ports 持续流丢包 8.8% 被一票否决,详见 [ADR-001](docs/decisions/ADR-001-技术栈.md)。

## 路线图

| 里程碑 | 内容 | 状态 |
|---|---|---|
| M0 | 四栈 POC 实测选型(ADR-001) | ✅ 2026-11-01 |
| M1 复验门 | G1 GUI 编译 / G2 真机回环 / G3 MCU 灌流 | ✅ 2026-11-01 |
| M1 MVP 0.1.0 | 连接/收发/时间戳/日志/HEX/虚拟滚动/过滤/档案/GBK/便携分发 | ✅ tag v0.1.0 |
| P1 | CRC 面板、多标签、自动应答规则表、示波器(JustFloat/CSV) | ✅ 本地已实现(push 待网络) |
| M2+ | TCP/UDP、日志回放、winget/scoop 分发 | 📋 规划中 |
| 远期 | Lua 脚本、协议解码、虚拟串口、**MCP server(AI ready)**、Web 版 | 📋 规划中 |

## 仓库结构

```
docs/        调研报告(40+ 工具横评)、项目规划、ADR 决策记录
bench/       串口回环丢包基准(发版常设回归门)+ ESP32 测试固件
poc/         四候选栈技术验证工程(保留作参考实现)
src/         正式代码
└── crates/portpilot-core/   核心层:ITransport 抽象、无锁 SPSC 环形缓冲、
                             自动重连会话、统计(11 单测;真机灌流延迟 p95 1.4ms)
```

## 开发

```bash
# 性能回归门(发版前必跑)
python bench/loopback_test.py --selftest                                  # 脚本自检
python bench/run_matrix.py --pair COM1 COM2 --seconds 5                   # 虚拟对矩阵
python bench/loopback_test.py --rx-only --rx COM9 --baud 460800 --seconds 30   # 真机灌流验收
```

POC 工程运行方式见 [poc/README.md](poc/README.md);ESP32 测试固件见 [bench/mcu_firmware/](bench/mcu_firmware/)。

## 文档

- [01-开源串口工具调研与功能对比报告](docs/01-开源串口工具调研与功能对比报告.md) —— 40+ 工具横评、16×16 功能矩阵
- [02-项目规划](docs/02-项目规划.md) —— 定位、P0/P1/P2、里程碑、风险
- [ADR-001-技术栈](docs/decisions/ADR-001-技术栈.md) —— 选型证据链与踩坑记录

## License

[Apache-2.0](LICENSE)
