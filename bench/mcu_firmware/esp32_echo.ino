// PortPilot G2/G3 · ESP32 透明回显固件
// =====================================
// 用途:配合 bench/run_matrix.py --loop COMx 做"真实 MCU 在环"回环测试。
// PC(CH340 模块)发什么,ESP32 就原样回什么,数据穿过两圈完整链路。
//
// 接线(3.3V 逻辑!):
//   CH340 模块 TXD -> ESP32 RX (默认 IO3 / RX)
//   CH340 模块 RXD -> ESP32 TX (默认 IO1 / TX)
//   CH340 模块 GND -> ESP32 GND
//   5V / VCC / 3V3 悬空(ESP32 自行供电,USB 口即可)
//
// 注意:UART 波特率由本固件的 UART_BAUD 决定,PC 端 bench 必须用同一速率:
//   python bench/run_matrix.py --loop COM12 --bauds 460800 --seconds 10
// 换速率测试(G3b 探索 921600)时,改 UART_BAAD 重新烧录。

#define UART_BAUD 460800   // 改这里:460800(P0 验收线)/ 921600(G3b 探索)

void setup() {
  Serial.begin(UART_BAUD);
}

void loop() {
  // 透明回显:逐块搬运,缓冲 4KB
  static uint8_t buf[4096];
  size_t n = Serial.available();
  if (n > 0) {
    if (n > sizeof(buf)) n = sizeof(buf);
    n = Serial.read(buf, n);
    if (n > 0) Serial.write(buf, n);
  }
}
