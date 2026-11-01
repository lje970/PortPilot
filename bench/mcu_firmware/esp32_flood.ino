// PortPilot G3 · ESP32 满速灌流固件(真实嵌入式流量源)
// =====================================================
// 用途:ESP32 按 UART_BAUD 满速连续发送 [4B 大端序号][0x55/0xAA payload] 数据块,
// PC 端纯接收校验丢包 —— 这正是 PortPilot 用户"DMA 高速回传"的真实场景。
//
// 接线(同 esp32_echo.ino):TXD->RX、RXD->TX(可选,用于双向)、GND->GND;电源悬空。
// PC 端命令:
//   python bench/loopback_test.py --rx-only --rx COM12 --baud 460800 --seconds 30
// 换速率:改 UART_BAUD 重新烧录,PC 端 --baud 同步改。

#define UART_BAUD 460800   // 改这里:460800(P0 验收线)/ 921600(G3b 探索)
#define PAYLOAD   252      // 块总长 256B(4B 序号 + 252B 校验体),与 bench 一致
#define RX_PIN    18       // ESP32-S3:接 CH340 模块 TXD
#define TX_PIN    17       // ESP32-S3:接 CH340 模块 RXD(可选)

static HardwareSerial FloodSerial(1);  // S3 上用 UART1(UART0 = 板载 USB 日志口)

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
