#!/usr/bin/env python3
"""串口回环丢包基准 —— 本工作区的统一量尺(M0/bench)。

原理
  TX 线程向 tx 端口发送定长块: [4B 大端序号][0x55/0xAA 交替 payload]
  RX 线程在 rx 端口流式读取并重组序号:
    序号连续        -> blocks_ok++
    序号跳变 +N     -> blocks_lost += N          (丢块)
    payload 校验错  -> blocks_err++              (错块)
    序号回退        -> seq_regress++             (乱序/重复)
  结束输出 JSON 指标。链路用虚拟对(如 COM1<->COM2)时链路层零丢包,
  任何差异都应归因于被测程序自身的管线。

用法
  python loopback_test.py --tx COM2 --rx COM1 --baud 921600 --seconds 10
  python loopback_test.py --loop COM12 --baud 921600 --seconds 10   # 单口回环(TX-RX 短接)
  python loopback_test.py --selftest                                # loop:// 软件冒烟,验证脚本自身
"""

import argparse
import json
import sys
import threading
import time

import serial

BLOCK_HEADER = 4
DEFAULT_PAYLOAD = 252          # 块总长 256B
STOP_SENTINEL = b"\x00\xFF\x10\x04BENCH_END\xff\xff\xff\xff"


def make_block(seq: int, payload: bytes) -> bytes:
    return seq.to_bytes(4, "big") + payload


class Stats:
    def __init__(self):
        self.lock = threading.Lock()
        self.tx_bytes = 0
        self.tx_blocks = 0
        self.rx_bytes = 0
        self.blocks_ok = 0
        self.blocks_lost = 0
        self.blocks_err = 0
        self.seq_regress = 0
        self.expect = None
        self.buf = bytearray()

    def feed(self, data: bytes, block_size: int, payload: bytes):
        with self.lock:
            self.rx_bytes += len(data)
            self.buf.extend(data)
            while True:
                if len(self.buf) < BLOCK_HEADER:
                    break
                seq = int.from_bytes(self.buf[:BLOCK_HEADER], "big")
                if len(self.buf) < block_size:
                    break
                body = bytes(self.buf[BLOCK_HEADER:block_size])
                del self.buf[:block_size]
                if body != payload:
                    self.blocks_err += 1
                if self.expect is None:
                    self.expect = seq + 1
                    self.blocks_ok += 1
                elif seq == self.expect:
                    self.blocks_ok += 1
                    self.expect += 1
                elif seq > self.expect:
                    self.blocks_lost += seq - self.expect
                    self.blocks_ok += 1
                    self.expect = seq + 1
                else:  # seq < expect:重复或乱序
                    self.seq_regress += 1


def run_pair(tx_port: str, rx_port: str, baud: int, seconds: float,
             block_size: int, verbose: bool) -> dict:
    payload = bytes((0x55, 0xAA)) * ((block_size - BLOCK_HEADER) // 2)
    st = Stats()
    stop = threading.Event()

    try:
        ser_tx = serial.Serial(tx_port, baud, timeout=0.1, write_timeout=2.0)
        ser_rx = serial.Serial(rx_port, baud, timeout=0.1)
    except serial.SerialException as e:
        print(f"[bench] 打开端口失败: {e}", file=sys.stderr)
        sys.exit(2)

    def tx_loop():
        seq = 0
        deadline = time.monotonic() + seconds
        while not stop.is_set() and time.monotonic() < deadline:
            try:
                n = ser_tx.write(make_block(seq, payload))
            except serial.SerialTimeoutException:
                break
            with st.lock:
                st.tx_bytes += n
                st.tx_blocks += 1
            seq += 1
        try:  # 结束哨兵,给 RX 一个明确的收尾标记
            ser_tx.write(STOP_SENTINEL)
        except Exception:
            pass

    def rx_loop():
        deadline = time.monotonic() + seconds + 1.5  # 多收 1.5s 排水
        while not stop.is_set() and time.monotonic() < deadline:
            data = ser_rx.read(4096)
            if STOP_SENTINEL in data:
                stop.set()
            if data:
                st.feed(data, block_size, payload)

    t0 = time.monotonic()
    threads = [threading.Thread(target=tx_loop), threading.Thread(target=rx_loop)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    duration = time.monotonic() - t0
    ser_tx.close()
    ser_rx.close()

    return report(st, duration, "pair", tx_port, rx_port, baud, block_size, verbose)


def run_generator(tx_port: str, baud: int, seconds: float, block_size: int, verbose: bool) -> dict:
    """纯发生器模式:只灌流不接收(链路另一端由被测 POC 自行接收统计)。"""
    payload = bytes((0x55, 0xAA)) * ((block_size - BLOCK_HEADER) // 2)
    try:
        ser = serial.Serial(tx_port, baud, timeout=0.1, write_timeout=2.0)
    except serial.SerialException as e:
        print(f"[bench] 打开端口失败: {e}", file=sys.stderr)
        sys.exit(2)
    seq = 0
    tx_bytes = 0
    t0 = time.monotonic()
    deadline = t0 + seconds
    while time.monotonic() < deadline:
        try:
            tx_bytes += ser.write(make_block(seq, payload))
        except serial.SerialTimeoutException:
            break
        seq += 1
    duration = time.monotonic() - t0
    ser.close()
    result = {
        "mode": "generator", "tx_port": tx_port, "baud": baud,
        "block_bytes": block_size, "seconds": round(duration, 3),
        "tx_bytes": tx_bytes, "tx_blocks": seq,
        "ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }
    if verbose:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    print(f"[gen] 灌流 {tx_bytes}B / {seq} 块 ({duration:.1f}s)", file=sys.stderr)
    return result


def run_loop(port: str, baud: int, seconds: float, block_size: int, verbose: bool) -> dict:
    """单口回环:TX-RX 已短接,同一 serial 对象一读一写。"""
    payload = bytes((0x55, 0xAA)) * ((block_size - BLOCK_HEADER) // 2)
    st = Stats()
    try:
        ser = serial.Serial(port, baud, timeout=0.1, write_timeout=2.0)
    except serial.SerialException as e:
        print(f"[bench] 打开端口失败: {e}", file=sys.stderr)
        sys.exit(2)

    stop = threading.Event()
    deadline = time.monotonic() + seconds

    def tx_loop():
        seq = 0
        while not stop.is_set() and time.monotonic() < deadline:
            try:
                n = ser.write(make_block(seq, payload))
            except serial.SerialTimeoutException:
                break
            with st.lock:
                st.tx_bytes += n
                st.tx_blocks += 1
            seq += 1
        try:
            ser.write(STOP_SENTINEL)
        except Exception:
            pass

    def rx_loop():
        end = deadline + 1.5
        while not stop.is_set() and time.monotonic() < end:
            data = ser.read(4096)
            if STOP_SENTINEL in data:
                stop.set()
            if data:
                st.feed(data, block_size, payload)

    t0 = time.monotonic()
    threads = [threading.Thread(target=tx_loop), threading.Thread(target=rx_loop)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    duration = time.monotonic() - t0
    ser.close()
    return report(st, duration, "loop", port, port, baud, block_size, verbose)


def run_rx_only(rx_port: str, baud: int, seconds: float, block_size: int, verbose: bool) -> dict:
    """纯接收校验模式:对端由 MCU 固件主动灌流(见 bench/mcu_firmware/esp32_flood),
    本端只收并按序号连续性统计丢块。固件速率未知,以实际收到为准;
    首块对齐:逐字节滑动寻找 [4B 序号][payload] 合法块。"""
    payload = bytes((0x55, 0xAA)) * ((block_size - BLOCK_HEADER) // 2)
    try:
        ser = serial.Serial(rx_port, baud, timeout=0.1)
    except serial.SerialException as e:
        print(f"[bench] 打开端口失败: {e}", file=sys.stderr)
        sys.exit(2)

    st = Stats()
    buf = bytearray()
    ok = lost = err = 0
    expect = None
    t0 = time.monotonic()
    deadline = t0 + seconds
    while time.monotonic() < deadline:
        data = ser.read(65536)
        if not data:
            continue
        st.rx_bytes += len(data)
        buf.extend(data)
        # 滑动对齐 + 连续校验
        while len(buf) >= block_size:
            seq = int.from_bytes(buf[:BLOCK_HEADER], "big")
            if bytes(buf[BLOCK_HEADER:block_size]) == payload:
                del buf[:block_size]
                if expect is None:
                    expect = seq + 1
                    ok += 1
                elif seq == expect:
                    ok += 1
                    expect += 1
                elif seq > expect:
                    lost += seq - expect
                    ok += 1
                    expect = seq + 1
                else:
                    st.seq_regress += 1
            else:
                del buf[:1]  # 未对齐,滑动一字节
    duration = time.monotonic() - t0
    ser.close()
    with st.lock:
        result = {
            "mode": "rx-only", "rx_port": rx_port, "baud": baud,
            "block_bytes": block_size, "seconds": round(duration, 3),
            "rx_bytes": st.rx_bytes, "blocks_ok": ok, "blocks_lost": lost,
            "blocks_err": err, "seq_regress": st.seq_regress,
            "loss_pct": round(100.0 * lost / max(1, ok + lost), 4),
            "effective_bps": int(st.rx_bytes * 8 / duration) if duration > 0 else 0,
            "ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
        }
    if verbose:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    verdict = "OK" if lost == 0 and st.seq_regress == 0 else "LOSS/ERR"
    print(f"[bench] {verdict} rx={st.rx_bytes}B thr={result['effective_bps']}bps "
          f"lost={lost} regress={st.seq_regress} ({duration:.1f}s)", file=sys.stderr)
    return result


def run_selftest(seconds: float = 3.0) -> dict:
    """pyserial loop:// 软件回环冒烟:只验证脚本逻辑与 pyserial 安装。"""
    payload = bytes((0x55, 0xAA)) * ((256 - BLOCK_HEADER) // 2)
    st = Stats()
    ser = serial.serial_for_url("loop://", timeout=0.1, write_timeout=2.0)
    stop = threading.Event()
    deadline = time.monotonic() + seconds

    def tx_loop():
        seq = 0
        while not stop.is_set() and time.monotonic() < deadline:
            n = ser.write(make_block(seq, payload))
            with st.lock:
                st.tx_bytes += n
                st.tx_blocks += 1
            seq += 1
        ser.write(STOP_SENTINEL)

    def rx_loop():
        end = deadline + 1.0
        while not stop.is_set() and time.monotonic() < end:
            data = ser.read(4096)
            if STOP_SENTINEL in data:
                stop.set()
            if data:
                st.feed(data, 256, payload)

    t0 = time.monotonic()
    threads = [threading.Thread(target=tx_loop), threading.Thread(target=rx_loop)]
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    ser.close()
    return report(st, time.monotonic() - t0, "selftest", "loop://", "loop://", 0, 256, True)


def report(st: Stats, duration: float, mode, tx_port, rx_port, baud, block_size, verbose) -> dict:
    with st.lock:
        result = {
            "mode": mode, "tx_port": tx_port, "rx_port": rx_port, "baud": baud,
            "block_bytes": block_size, "seconds": round(duration, 3),
            "tx_bytes": st.tx_bytes, "tx_blocks": st.tx_blocks,
            "rx_bytes": st.rx_bytes,
            "blocks_ok": st.blocks_ok, "blocks_lost": st.blocks_lost,
            "blocks_err": st.blocks_err, "seq_regress": st.seq_regress,
            "loss_pct": round(100.0 * st.blocks_lost / max(1, st.blocks_ok + st.blocks_lost), 4),
            "effective_bps": int(st.rx_bytes * 8 / duration) if duration > 0 else 0,
            "ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
        }
    if verbose:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    verdict = "OK" if result["blocks_lost"] == 0 and result["blocks_err"] == 0 else "LOSS/ERR"
    print(f"[bench] {verdict} rx={result['rx_bytes']}B "
          f"thr={result['effective_bps']}bps lost={result['blocks_lost']} "
          f"err={result['blocks_err']} ({duration:.1f}s)", file=sys.stderr)
    return result


def main():
    ap = argparse.ArgumentParser(description="串口回环丢包基准")
    ap.add_argument("--tx", help="发送端端口(虚拟对/交叉线的写入口)")
    ap.add_argument("--rx", help="接收端端口")
    ap.add_argument("--loop", help="单口回环模式(TX-RX 短接)")
    ap.add_argument("--baud", type=int, default=921600)
    ap.add_argument("--seconds", type=float, default=10.0)
    ap.add_argument("--block", type=int, default=256, help="块字节数(含 4B 序号)")
    ap.add_argument("--json", help="结果追加写入的 JSONL 文件")
    ap.add_argument("--tx-only", action="store_true", help="纯发生器模式:只向 --tx 灌流,不接收")
    ap.add_argument("--rx-only", action="store_true", help="纯接收校验模式:对端由 MCU 固件灌流")
    ap.add_argument("--selftest", action="store_true", help="loop:// 软件冒烟")
    args = ap.parse_args()

    if args.selftest:
        result = run_selftest()
    elif args.rx_only and args.rx:
        result = run_rx_only(args.rx, args.baud, args.seconds, args.block, True)
    elif args.tx_only and args.tx:
        result = run_generator(args.tx, args.baud, args.seconds, args.block, True)
    elif args.loop:
        result = run_loop(args.loop, args.baud, args.seconds, args.block, True)
    elif args.tx and args.rx:
        result = run_pair(args.tx, args.rx, args.baud, args.seconds, args.block, True)
    else:
        ap.error("需要 --tx/--rx,或 --loop,或 --selftest")
        return

    if args.json:
        with open(args.json, "a", encoding="utf-8") as f:
            f.write(json.dumps(result, ensure_ascii=False) + "\n")


if __name__ == "__main__":
    main()
