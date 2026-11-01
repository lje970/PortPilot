// PortPilot P1 · ESP32 JustFloat 示波器信号源
// ===========================================
// 以 100Hz 发送 JustFloat 帧:2 通道正弦波(幅值 100 / 50,相位差 90°)
// 帧 = [f32 LE × N][00 00 80 7F],与 PortPilot 示波器面板(JustFloat 协议)配套。
//
// 接线:GPIO17(TX2) -> CH340 模块 RXD;GND 共地;模块 5V/VCC/3V3 悬空。
// 示波器面板设置:协议 JustFloat · 通道 2 · 勾选"启用抽头"。

#include <Arduino.h>
#include <math.h>

#define UART_BAUD 460800
#define FRAME_HZ  100
#define TX_PIN    17   // S3 UART1 TX(经典 ESP32 改 17 同样可用)

static HardwareSerial ScopeSerial(1);
static uint32_t last_us = 0;
static uint32_t k = 0;

void setup() {
  ScopeSerial.begin(UART_BAUD, SERIAL_8N1, -1, TX_PIN);
  last_us = micros();
}

void loop() {
  const uint32_t period = 1000000 / FRAME_HZ;
  if (micros() - last_us < period) return;
  last_us += period;

  float a = 100.0f * sinf(k * 0.06f);
  float b = 50.0f * cosf(k * 0.06f);
  ScopeSerial.write((uint8_t *)&a, 4);
  ScopeSerial.write((uint8_t *)&b, 4);
  const uint8_t tail[4] = {0x00, 0x00, 0x80, 0x7F};
  ScopeSerial.write(tail, 4);
  k++;
}
