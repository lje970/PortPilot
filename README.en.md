# PortPilot

> A modern open-source serial debugging workbench for embedded developers — terminal, real-time oscilloscope and protocol decoding in one place.

**Status: M1/M2 development (v0.2.0 test release)** · Stack: **Tauri 2 + Rust** · License: **Apache-2.0** · Platform: Windows 10/11 first (cross-platform-ready architecture)

[中文说明](README.md)

## Why PortPilot

In 2026 the serial-debug tool landscape: closed-source classics (SSCOM, Serial Hunter) are unmaintained; the leading open-source project (Serial Studio 4.x) went dual-license with paid Pro features; terminal suites (Tabby/WindTerm) treat serial as a shallow side feature; and oscilloscope tools (VOFA+/SerialPlot) are disconnected from the terminal. **"Modern UI + high-throughput stability + zero-script automation + all-in-one + fully free open source" has no incumbent** — PortPilot fills that gap.

Commitment: **all core features free and open source forever. No Pro tier.**

## Verified metrics (measured, reproducible)

| Metric | Value | Method |
|---|---|---|
| 460800 baud sustained stream | **zero packet loss** (5631 frames / 30 s, real MCU flood) | ESP32-S3 flood + sequence check ([bench](bench/)) |
| Hardware loopback matrix | 115200–921600 all zero loss; 460800 at 99.1 % line rate | CH340 hardware loopback |
| Pipeline latency (arrival→display) p95 | **0.23 ms** (console) / 3.43 ms (GUI) | unified probe protocol |
| Memory | core process **14–29 MB** (Electron same-scenario: 430 MB) | four-stack POC comparison |
| Portable artifact | **527 KB single exe** (core) / 8.7 MB (GUI) | cargo release |

## Features (v0.2.0)

- Serial / **TCP client / TCP server / UDP** transports (same session pipeline)
- Text + HEX dual-mode send/receive, CR/LF/CRLF control
- Timestamps, pause, clear, auto-scroll, live RX/TX stats
- **1,000,000-line virtual-scrolled terminal** with keyword/regex filtering and highlight
- **Multi-tab sessions** with independent filters and encodings
- **Auto-response rule table** (text/HEX/regex match → send, zero scripting)
- **Oscilloscope** panel: JustFloat (VOFA+-compatible) and CSV-line protocols
- **CRC send panel**: SUM8/XOR8/CRC16-Modbus/CCITT/XMODEM/CRC32 with endianness
- DTR/RTS line control, auto-reconnect, raw stream logging (`logs/*.bin`)
- **Log replay** at 1×/10×/100×/max speed
- UTF-8/GBK encoding switch (hot), connection profiles, dark theme
- Portable exe, no installer required

## Roadmap

| Milestone | Scope | Status |
|---|---|---|
| M0 | Four-stack POC → stack decision (ADR-001) | ✅ |
| M1 (v0.1.0) | MVP backbone + P0 checklist | ✅ |
| P1 (v0.2.0) | CRC panel, multi-tab, rule table, oscilloscope | ✅ |
| M2 | TCP/UDP, log replay, package manager distribution | 🔨 in progress |
| P2 | Lua scripting, protocol decoding, virtual ports, **MCP server (AI-ready)**, web version | 📋 planned |

## Development

```powershell
# build (vcvars64 environment required — see docs/03 §4)
powershell.exe -Command "cmd.exe /s /c '\"C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat\" >nul 2>&1 && cd /d <repo>\src && cargo build -p portpilot-app --release'"

# unit tests (25+, no hardware needed)
cd src && cargo test -p portpilot-core

# performance regression gate (release checklist)
python bench/loopback_test.py --selftest
```

See [docs/03-交接文档.md](docs/03-交接文档.md) (Chinese) for the full engineering handbook — architecture, pitfalls and verification data.

## License

[Apache-2.0](LICENSE)
