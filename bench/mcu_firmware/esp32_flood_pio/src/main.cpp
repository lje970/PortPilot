// PortPilot G3 · ESP32-S3 满速灌流(PlatformIO/Arduino 框架版)
// ===========================================================
// 用途:按 UART_BAUD 满速连续发送 [4B 大端序号][0x55/0xAA payload] 块,
// PC 端纯接收校验:python bench/loopback_test.py --rx-only --rx <COM号> --baud 460800 --seconds 30
//
// S3 的 UART 外设可任意引脚映射;选 GPIO17/18(避开 USB 口 GPIO19/20 与 strapping 脚)。
// 接线(3.3V 逻辑!):
//   CH340 模块 TXD -> ESP32 GPIO18 (RX)
//   CH340 模块 RXD -> ESP32 GPIO17 (TX)   // 可不接,纯接收用不到,但接上便于扩展
//   CH340 模块 GND -> ESP32 GND
//   5V / VCC / 3V3 悬空(板子自己 USB 供电)
//
// 换速率(G3b 探索 921600):改 UART_BAUD 重新 Upload,PC 端 --baud 同步改。

#include <Arduino.h>

#define UART_BAUD 460800   // 改这里:460800(P0 验收线)/ 921600(G3b 探索)
#define PAYLOAD   252      // 块总长 256B,与 bench/loopback_test.py 完全一致
#define RX_PIN    18       // 接 CH340 模块 TXD
#define TX_PIN    17       // 接 CH340 模块 RXD(可选)

static HardwareSerial FloodSerial(1);  // S3 上用 UART1(UART0 = 板载 USB 日志)
static uint32_t seq = 0;
static uint8_t block[4 + PAYLOAD];

static void fill_block() {
  block[0] = (seq >> 24) & 0xFF;
  block[1] = (seq >> 16) & 0xFF;
  block[2] = (seq >> 8) & 0xFF;
  block[3] = seq & 0xFF;
  for (int i = 0; i < PAYLOAD; i += 2) {
    block[4 + i] = 0x55;
    block[5 + i] = 0xAA;
  }
}

void setup() {
  FloodSerial.begin(UART_BAUD, SERIAL_8N1, RX_PIN, TX_PIN);
}

void loop() {
  fill_block();
  FloodSerial.write(block, sizeof(block));
  FloodSerial.flush();  // 等待硬件发完 -> 满速连续流(460800 ≈ 46KB/s ≈ 179 块/s)
  seq++;
}
