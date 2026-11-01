# POC 统一探针协议(v1)

四个候选栈的 POC 必须实现**完全相同**的命令行接口与探针输出,这样 `bench/run_poc_test.py` 才能公平对比。

## 命令行接口

```
<poc 运行命令> --port COM1 --baud 921600 --seconds 30 --out <路径>.jsonl
```

- `--port` 接收端串口(虚拟对写入口的对端)
- `--baud` 波特率(虚拟口下仅元数据)
- `--seconds` 收流时长;到时后**自行排水 ~1.5s 再退出**,退出码 0
- `--out` 探针 JSONL 输出路径(每秒追加一行)

## 必做行为

1. 打开端口失败→带明确错误退出(码 2);
2. 收流:I/O 线程(或异步回调)读取 → **批量/节流上屏**(≥30fps 目标),禁止逐字节刷 UI;
3. 断线(拔口/被占用):自动重连(≥500ms 退避),恢复参数,累计 `reconnects`;
4. 每秒向 `--out` 追加一行 JSON(见下);到时自动退出。

## JSONL 行格式(每秒一行)

```json
{"t": 3, "rx_bytes": 385024, "ui_fps": 30.0, "latency_p95_ms": 4.1, "inter_arrival_p95_ms": 2.9, "reconnects": 0}
```

| 字段 | 定义 | 口径 |
|---|---|---|
| `t` | 第几秒(从 1 开始) | — |
| `rx_bytes` | 累计接收字节数 | I/O 层读到的原始字节 |
| `ui_fps` | 该秒 UI 刷新帧率 | 每秒完成的上屏批次数(33ms 节流下应 ≈30) |
| `latency_p95_ms` | 该秒"数据到达→上屏"延迟 p95 | `t_display - t_arrival`,取该秒样本 95 分位 |
| `inter_arrival_p95_ms` | 该秒 I/O 读取间隔 p95 | 相邻两次 read/readyRead 返回的时间差 p95 |
| `reconnects` | 累计自动重连次数 | — |

> RSS / CPU 不由 POC 自己上报 —— 由 `run_poc_test.py` 用 psutil 统一从进程树外部采样(含 Electron 子进程),口径一致。
