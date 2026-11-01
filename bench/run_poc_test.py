#!/usr/bin/env python3
"""POC 统一测试跑台 —— 四栈公平对比的执行器。

场景(与 bench/README.md 一致):
    本跑台 --tx-port(如 COM2) --[Eltima 虚拟对]--> --rx-port(如 COM1) 被测 POC

流程:
  1) 启动被测 POC(--cmd;POC 按 poc/probe_protocol.md 自己写探针 JSONL)
  2) 同步启动 bench 纯发生器,向 --tx-port 灌流 --seconds 秒
  3) 每秒采样 POC 进程树 RSS/CPU(psutil,含子进程,对四栈口径一致)
  4) 结束读 POC 探针 JSONL,汇总:管线丢包率(tx_bytes - rx_bytes)/tx_bytes、
     延迟 p95、UI fps、RSS 峰/均值、CPU 均值 → 追加 CSV + 打印

用法:
  python run_poc_test.py --tag pyside6 \
      --cmd "python poc/pyside6_app/main.py --port COM1 --baud 921600 --seconds 30 --out poc/pyside6_app/probe_results.jsonl" \
      --probe-out poc/pyside6_app/probe_results.jsonl --seconds 30
"""

import argparse
import csv
import datetime
import json
import os
import subprocess
import sys
import time

import psutil

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from loopback_test import run_generator  # noqa: E402

FIELDS = ["tag", "seconds", "gen_tx_bytes", "poc_rx_bytes", "pipeline_loss_pct",
          "latency_p95_ms", "inter_arrival_p95_ms", "ui_fps_avg",
          "rss_avg_mb", "rss_peak_mb", "cpu_avg_pct", "reconnects", "verdict"]


def read_probe(path: str) -> dict:
    if not os.path.exists(path):
        return {}
    lines = []
    with open(path, encoding="utf-8") as f:
        for ln in f:
            ln = ln.strip()
            if ln:
                try:
                    lines.append(json.loads(ln))
                except json.JSONDecodeError:
                    pass
    if not lines:
        return {}
    agg = {
        "poc_rx_bytes": max(int(s.get("rx_bytes", 0)) for s in lines),
        "reconnects": sum(int(s.get("reconnects", 0)) for s in lines),
    }
    lat = sorted(float(s["latency_p95_ms"]) for s in lines if "latency_p95_ms" in s)
    iar = sorted(float(s["inter_arrival_p95_ms"]) for s in lines if "inter_arrival_p95_ms" in s)
    fps = [float(s["ui_fps"]) for s in lines if "ui_fps" in s]
    if lat:
        agg["latency_p95_ms"] = round(lat[int(len(lat) * 0.95) - 1 if len(lat) > 1 else 0], 2)
    if iar:
        agg["inter_arrival_p95_ms"] = round(iar[int(len(iar) * 0.95) - 1 if len(iar) > 1 else 0], 2)
    if fps:
        agg["ui_fps_avg"] = round(sum(fps) / len(fps), 1)
    return agg


def sample_tree(proc: psutil.Process):
    try:
        procs = [proc] + proc.children(recursive=True)
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        return None, None
    rss = cpu = 0.0
    for p in procs:
        try:
            rss += p.memory_info().rss
            cpu += p.cpu_percent(interval=None)
        except (psutil.NoSuchProcess, psutil.AccessDenied):
            pass
    return rss, cpu


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", required=True)
    ap.add_argument("--cmd", required=True, help="启动被测 POC 的完整命令行")
    ap.add_argument("--probe-out", required=True, help="POC 探针 JSONL 路径(用于读取结果)")
    ap.add_argument("--tx-port", default="COM2")
    ap.add_argument("--baud", type=int, default=921600)
    ap.add_argument("--seconds", type=float, default=30.0)
    ap.add_argument("--block", type=int, default=256)
    ap.add_argument("--force", action="store_true", help="覆盖已有探针文件")
    args = ap.parse_args()

    if args.force and os.path.exists(args.probe_out):
        os.remove(args.probe_out)

    if os.path.exists(args.probe_out):
        print(f"[run] 警告:探针文件已存在,将沿用({args.probe_out});加 --force 重测", file=sys.stderr)

    print(f"[run] 启动被测 POC: {args.cmd}")
    if os.path.exists(args.probe_out) and not args.force:
        pre_lines = sum(1 for _ in open(args.probe_out, encoding="utf-8"))
    else:
        pre_lines = 0

    poc = subprocess.Popen(args.cmd, shell=True)
    time.sleep(2.0)  # 等 POC 完成启动/打开串口
    try:
        proc = psutil.Process(poc.pid)
    except psutil.NoSuchProcess:
        print("[run] POC 进程启动即退出,失败", file=sys.stderr)
        sys.exit(3)

    rss_samples, cpu_samples = [], []
    gen_result = {}

    import threading

    def gen_thread():
        gen_result.update(run_generator(args.tx_port, args.baud, args.seconds,
                                        args.block, verbose=False))

    gt = threading.Thread(target=gen_thread)
    t0 = time.monotonic()
    gt.start()
    while gt.is_alive():
        rss, cpu = sample_tree(proc)
        if rss is not None:
            rss_samples.append(rss)
            cpu_samples.append(cpu)
        time.sleep(1.0)
    gt.join()
    elapsed = time.monotonic() - t0

    # 等 POC 自行按 --seconds 退出(它比发生器多 1~2s 排水);最多再等 8s
    try:
        poc.wait(timeout=8)
    except subprocess.TimeoutExpired:
        print("[run] POC 未按时退出,终止", file=sys.stderr)
        for c in psutil.Process(poc.pid).children(recursive=True) + [psutil.Process(poc.pid)]:
            try:
                c.kill()
            except (psutil.NoSuchProcess, psutil.AccessDenied):
                pass

    gen_tx = gen_result.get("tx_bytes", 0)
    # 只取本次测试新增的探针行
    if pre_lines:
        tmp = f"{args.probe_out}.tmp"
        with open(args.probe_out, encoding="utf-8") as fin, open(tmp, "w", encoding="utf-8") as fout:
            for i, ln in enumerate(fin):
                if i >= pre_lines:
                    fout.write(ln)
        os.replace(tmp, args.probe_out)
    agg = read_probe(args.probe_out)
    poc_rx = agg.get("poc_rx_bytes", 0)
    loss = round(100.0 * (gen_tx - poc_rx) / gen_tx, 3) if gen_tx else None

    row = {
        "tag": args.tag, "seconds": round(elapsed - 2.0, 1),
        "gen_tx_bytes": gen_tx, "poc_rx_bytes": poc_rx, "pipeline_loss_pct": loss,
        "latency_p95_ms": agg.get("latency_p95_ms"),
        "inter_arrival_p95_ms": agg.get("inter_arrival_p95_ms"),
        "ui_fps_avg": agg.get("ui_fps_avg"),
        "rss_avg_mb": round(sum(rss_samples) / len(rss_samples) / 1e6, 1) if rss_samples else None,
        "rss_peak_mb": round(max(rss_samples) / 1e6, 1) if rss_samples else None,
        "cpu_avg_pct": round(sum(cpu_samples) / len(cpu_samples), 1) if cpu_samples else None,
        "reconnects": agg.get("reconnects", 0),
        "verdict": "OK" if loss is not None and loss <= 0.5 else "LOSS",  # ≤0.5% 视为收尾在途残余
    }
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
    os.makedirs(out_dir, exist_ok=True)
    csv_path = os.path.join(out_dir, "poc_results.csv")
    new_file = not os.path.exists(csv_path)
    with open(csv_path, "a", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=FIELDS)
        if new_file:
            w.writeheader()
        w.writerow(row)
    print(json.dumps(row, ensure_ascii=False, indent=2))
    print(f"[run] 已追加至 {csv_path}")


if __name__ == "__main__":
    main()
