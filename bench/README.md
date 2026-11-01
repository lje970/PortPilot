# bench —— 串口回环丢包基准(工作区统一量尺)

POC 验收与后续版本性能回归的**唯一性能裁决依据**(规划书 §5.2/§4.4)。

## 组成

| 文件 | 作用 |
|---|---|
| `loopback_test.py` | 核心量尺:序号块[TX]→[RX]重组,统计丢块/错块/吞吐 |
| `run_matrix.py` | 波特率矩阵批量测试,汇总 CSV + Markdown |
| `probe_ch340.py` | 探测真实 USB 口是否带 TX-RX 短接回环 |
| `results/` | 测试结果(CSV/MD/JSONL) |

## 测量原理

发送端发定长块 `[4B 大端序号][0x55/0xAA payload]`;接收端按序号连续性判定:
序号跳变 = 丢块,payload 不符 = 错块。**虚拟对(COM1↔COM2)链路层零丢包**,
因此"发生器 → 虚拟对 → 被测 POC"场景中,POC 收到的字节数与发生器发送数之差
= POC 自身管线丢包,归因干净。

## 标准测试场景(四栈 POC 统一)

```
bench 发生器 --COM2--> [Eltima 虚拟对] --COM1--> 被测 POC(921600,N 秒)
```

- 链路层零丢包由 `run_matrix --pair COM1 COM2` 预先确认;
- POC 收流统计来自其内置探针(`poc/probe_protocol.md`),对比 TX 字节数即得各栈管线丢包率。

## 常用命令

```bash
# 0) 脚本自身冒烟(不依赖任何硬件)
python bench/loopback_test.py --selftest

# 1) 虚拟对链路验证 + 矩阵
python bench/run_matrix.py --pair COM1 COM2 --seconds 5

# 2) 真实硬件(CH340 需 TX-RX 短接)
python bench/probe_ch340.py COM12
python bench/run_matrix.py --loop COM12 --seconds 10

# 3) 单次双口测试 + JSONL 归档
python bench/loopback_test.py --tx COM2 --rx COM1 --baud 921600 --seconds 10 --json bench/results/bench.jsonl
```

## 指标口径

| 指标 | 含义 |
|---|---|
| `effective_bps` | 实测吞吐(收字节×8÷时长);真实硬件上 ≈ baud/10 为满速 |
| `blocks_lost / loss_pct` | 丢块数 / 丢块率(序号跳变累计) |
| `blocks_err` | payload 校验错块(误码) |
| `seq_regress` | 乱序/重复(正常应为 0) |
