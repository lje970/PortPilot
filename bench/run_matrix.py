#!/usr/bin/env python3
"""波特率 × 时长测试矩阵:批量跑 loopback_test 并汇总 CSV + Markdown 表。

用法
  python run_matrix.py --pair COM1 COM2 --bauds 115200 921600 3000000 --seconds 5
  python run_matrix.py --loop COM12 --bauds 921600 --seconds 10
结果写入 bench/results/matrix_<时间戳>.csv 与 .md
"""

import argparse
import csv
import datetime
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from loopback_test import run_pair, run_loop  # noqa: E402

FIELDS = ["baud", "seconds", "tx_bytes", "rx_bytes", "effective_bps",
          "blocks_ok", "blocks_lost", "blocks_err", "seq_regress", "loss_pct"]


def main():
    ap = argparse.ArgumentParser()
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--pair", nargs=2, metavar=("TX", "RX"))
    g.add_argument("--loop", metavar="PORT")
    ap.add_argument("--bauds", type=int, nargs="+",
                    default=[115200, 460800, 921600, 1000000, 2000000, 3000000])
    ap.add_argument("--seconds", type=float, default=5.0)
    ap.add_argument("--block", type=int, default=256)
    ap.add_argument("--tag", default="", help="结果文件附加标签(如栈名)")
    args = ap.parse_args()

    rows = []
    for baud in args.bauds:
        print(f"\n===== baud={baud} =====", file=sys.stderr)
        try:
            if args.pair:
                r = run_pair(args.pair[0], args.pair[1], baud, args.seconds, args.block, verbose=False)
            else:
                r = run_loop(args.loop, baud, args.seconds, args.block, verbose=False)
        except SystemExit:
            print(f"[matrix] baud={baud} 打开失败,跳过", file=sys.stderr)
            continue
        rows.append({k: r.get(k) for k in FIELDS})

    stamp = datetime.datetime.now().strftime("%Y%m%d_%H%M%S")
    tag = f"_{args.tag}" if args.tag else ""
    out_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "results")
    os.makedirs(out_dir, exist_ok=True)
    csv_path = os.path.join(out_dir, f"matrix_{stamp}{tag}.csv")
    md_path = os.path.join(out_dir, f"matrix_{stamp}{tag}.md")

    with open(csv_path, "w", newline="", encoding="utf-8") as f:
        w = csv.DictWriter(f, fieldnames=FIELDS)
        w.writeheader()
        w.writerows(rows)

    with open(md_path, "w", encoding="utf-8") as f:
        f.write(f"# 串口回环基准矩阵({args.tag or 'bench'}){datetime.datetime.now():%Y-%m-%d %H:%M}\n\n")
        mode = f"pair {args.pair}" if args.pair else f"loop {args.loop}"
        f.write(f"模式:`{mode}` · 每档 {args.seconds}s · 块 {args.block}B\n\n")
        f.write("| " + " | ".join(FIELDS) + " |\n")
        f.write("|" + "---|" * len(FIELDS) + "\n")
        for r in rows:
            f.write("| " + " | ".join(str(r[k]) for k in FIELDS) + " |\n")

    print(f"\n[matrix] 结果已写入:\n  {csv_path}\n  {md_path}")
    for r in rows:
        flag = "OK" if r["blocks_lost"] == 0 and r["blocks_err"] == 0 else "<<< LOSS/ERR"
        print(f"  baud={r['baud']:>8} thr={r['effective_bps']:>10}bps "
              f"lost={r['blocks_lost']} err={r['blocks_err']} {flag}")


if __name__ == "__main__":
    main()
