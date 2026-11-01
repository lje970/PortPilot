#!/usr/bin/env python3
"""POC-B:PySide6(Qt 6)+ QSerialPort 最小串口收流工程。

架构:worker 线程持有 QSerialPort(readyRead 积累),33ms 定时器批量 emit;
GUI 线程只做节流渲染与探针统计。遵循 poc/probe_protocol.md v1。
"""

import argparse
import json
import sys
import time

from PySide6.QtCore import (QMetaObject, QObject, Qt, QThread, QTimer, Signal,
                            Slot, QElapsedTimer)
from PySide6.QtSerialPort import QSerialPort, QSerialPortInfo
from PySide6.QtWidgets import (QApplication, QLabel, QMainWindow, QPlainTextEdit,
                               QPushButton, QVBoxLayout, QWidget)

FLUSH_MS = 33          # UI 节流目标 30fps
LOG_MAX_BYTES = 64     # 每批预览的最大十六进制字节


class SerialWorker(QObject):
    batch = Signal(bytes, float, float, float, float)  # data, arr_oldest, arr_newest, iar_p95, now
    closed = Signal(int)                                # reconnects

    def __init__(self, port_name, baud):
        super().__init__()
        self.port_name = port_name
        self.baud = baud
        self.ser = None
        self.reconnects = 0
        self.buf = bytearray()
        self.arrivals = []
        self.last_read_ts = None
        self.iar_samples = []
        self.quit_flag = False

    @Slot()
    def begin(self):
        self.ser = QSerialPort(self)
        self.ser.readyRead.connect(self.on_ready)
        self.ser.errorOccurred.connect(self.on_error)
        self.flush_timer = QTimer(self)
        self.flush_timer.setInterval(FLUSH_MS)
        self.flush_timer.timeout.connect(self.on_flush)
        # 轮询兜底:readyRead 在高流量下可能滞后,2ms 主动抽干降低 OS 缓冲溢出概率
        self.poll_timer = QTimer(self)
        self.poll_timer.setInterval(2)
        self.poll_timer.timeout.connect(self.on_ready)
        self._open()
        self.flush_timer.start()
        self.poll_timer.start()

    def _open(self):
        self.ser.setPortName(self.port_name)
        self.ser.setBaudRate(self.baud)
        if self.ser.open(QSerialPort.ReadWrite):
            self.ser.clear()
            print(f"[pyside6] 已打开 {self.port_name} @ {self.baud}", flush=True)
        else:
            print(f"[pyside6] 打开 {self.port_name} 失败: {self.ser.errorString()}", file=sys.stderr)
            sys.exit(2)

    @Slot()
    def on_ready(self):
        now = time.perf_counter()
        data = bytes(self.ser.readAll())
        if not data:
            return
        if self.last_read_ts is not None:
            self.iar_samples.append((now - self.last_read_ts) * 1000.0)
        self.last_read_ts = now
        self.buf.extend(data)
        self.arrivals.append((now, now))  # (oldest, newest) 同批

    @Slot()
    def on_error(self, error):
        if error in (QSerialPort.ResourceError, QSerialPort.DeviceNotFoundError,
                     QSerialPort.PermissionError):
            print(f"[pyside6] 串口异常({self.ser.errorString()}),500ms 后重连", flush=True)
            self.ser.close()
            QTimer.singleShot(500, self._reopen)

    def _reopen(self):
        self.reconnects += 1
        self._open()

    @Slot()
    def on_flush(self):
        if self.buf:
            data = bytes(self.buf)
            arr_oldest = self.arrivals[0][0]
            arr_newest = self.arrivals[-1][1]
            iar = sorted(self.iar_samples)
            iar_p95 = iar[int(len(iar) * 0.95) - 1] if iar else 0.0
            self.buf = bytearray()
            self.arrivals = []
            self.iar_samples = []
            self.batch.emit(data, arr_oldest, arr_newest, iar_p95, time.perf_counter())

    @Slot()
    def finish(self):
        self.quit_flag = True
        self.flush_timer.stop()
        self.poll_timer.stop()
        if self.ser and self.ser.isOpen():
            self.ser.close()
        self.closed.emit(self.reconnects)


class MainWindow(QMainWindow):
    def __init__(self, port_name, baud, seconds, out_path):
        super().__init__()
        self.setWindowTitle("POC-B PySide6")
        self.out_path = out_path
        self.rx_bytes = 0
        self.lat_samples = []
        self.iar_window = []
        self.renders = 0
        self.reconnects = 0

        self.counter = QLabel("0 B")
        self.counter.setStyleSheet("font-size: 28px; font-weight: bold;")
        self.log = QPlainTextEdit()
        self.log.setReadOnly(True)
        self.log.setMaximumBlockCount(400)
        self.btn = QPushButton("暂停")
        lay = QVBoxLayout()
        lay.addWidget(self.counter)
        lay.addWidget(self.log)
        lay.addWidget(self.btn)
        central = QWidget()
        central.setLayout(lay)
        self.setCentralWidget(central)

        self.thread = QThread(self)
        self.worker = SerialWorker(port_name, baud)
        self.worker.moveToThread(self.thread)
        self.thread.started.connect(self.worker.begin)
        self.worker.batch.connect(self.on_batch)
        self.thread.start()

        self.clock = QElapsedTimer()
        self.clock.start()
        self.report = QTimer(self)
        self.report.setInterval(1000)
        self.report.timeout.connect(self.on_second)
        self.report.start()
        self.out_f = open(out_path, "a", encoding="utf-8")
        QTimer.singleShot(int((seconds + 5.0) * 1000), self.finish)

    @Slot(bytes, float, float, float, float)
    def on_batch(self, data, arr_oldest, arr_newest, iar_p95, now):
        self.rx_bytes += len(data)
        self.renders += 1
        self.lat_samples.append((now - arr_newest) * 1000.0)
        self.iar_window.append(iar_p95)
        self.counter.setText(f"{self.rx_bytes} B")
        self.log.appendPlainText(data[:LOG_MAX_BYTES].hex(" ") +
                                 (" …" if len(data) > LOG_MAX_BYTES else ""))

    @staticmethod
    def _p95(samples):
        if not samples:
            return 0.0
        s = sorted(samples)
        return s[max(0, int(len(s) * 0.95) - 1)]

    @Slot()
    def on_second(self):
        t = self.clock.elapsed() // 1000
        line = {"t": t, "rx_bytes": self.rx_bytes, "ui_fps": float(self.renders),
                "latency_p95_ms": round(self._p95(self.lat_samples), 2),
                "inter_arrival_p95_ms": round(self._p95(self.iar_window), 2),
                "reconnects": self.reconnects}
        self.out_f.write(json.dumps(line) + "\n")
        self.out_f.flush()
        self.setWindowTitle(f"POC-B PySide6 · {t}s · {self.renders} fps")
        self.lat_samples = []
        self.iar_window = []
        self.renders = 0

    def finish(self):
        # worker 的定时器属于 worker 线程,必须以队列方式在其线程内停止
        QMetaObject.invokeMethod(self.worker, "finish", Qt.QueuedConnection)
        self.thread.quit()
        self.thread.wait(2000)
        self.out_f.close()
        QApplication.quit()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", required=True)
    ap.add_argument("--baud", type=int, default=921600)
    ap.add_argument("--seconds", type=float, default=30)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    names = [p.portName() for p in QSerialPortInfo.availablePorts()]
    if args.port not in names:
        print(f"[pyside6] 端口 {args.port} 不在枚举中(现有:{names})", file=sys.stderr)
    app = QApplication(sys.argv)
    win = MainWindow(args.port, args.baud, args.seconds, args.out)
    win.resize(520, 420)
    win.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
