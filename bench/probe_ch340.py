#!/usr/bin/env python3
"""探测 CH340(COM12 等 USB 串口)是否带 TX→RX 短接回环头。

向端口发若干试探字节,监听回声。任何回声即判"疑似回环"
(若口上接着真实设备且恰好应答,需人工确认)。
发送内容为无指令含义的 0x55/0xAA,对常见从机设备无害。

用法: python probe_ch340.py COM12
"""

import sys
import time

import serial


def main():
    port_name = sys.argv[1] if len(sys.argv) > 1 else "COM12"
    try:
        ser = serial.Serial(port_name, 115200, timeout=0.15)
    except serial.SerialException as e:
        print(f"[probe] 打开 {port_name} 失败: {e}")
        sys.exit(2)
    ser.reset_input_buffer()
    echo_hits = 0
    for i in range(8):
        ser.write(b"\x55\xAA\x55\xAA\x55\xAA\x55\xAA")
        ser.flush()
        time.sleep(0.12)
        data = ser.read(64)
        if data:
            echo_hits += 1
            print(f"[probe] #{i} 回声 {len(data)}B: {data[:16].hex()}")
    ser.close()
    if echo_hits >= 3:
        print(f"[probe] 结论:{port_name} 疑似 TX-RX 回环在位(命中 {echo_hits}/8),可跑 --loop 真实硬件基准")
    else:
        print(f"[probe] 结论:{port_name} 未见回环(命中 {echo_hits}/8),需要 TX-RX 短接头后重测")


if __name__ == "__main__":
    main()
