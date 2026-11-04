// PortPilot P3 · 最小 SEGGER RTT 控制块(手写,probe-rs 兼容布局)
// 每 200ms 写一条 "Hello from RTT #N" 到 up-buffer,JTAG 侧直读 RAM 验证。
// 布局对齐 probe-rs rtt::Rtt 扫描预期:ID "SEGGER RTT" + Up/Down 描述符。
#include <Arduino.h>
#include <string.h>

struct RTTBufDesc {
  const char *name;
  char *buf;
  unsigned size;
  volatile unsigned wrOff;
  volatile unsigned rdOff;
  unsigned flags;
};

struct RTTControlBlock {
  char id[16];            // "SEGGER RTT\0\0\0\0\0"
  int maxUp;
  int maxDown;
  RTTBufDesc up[1];
  RTTBufDesc down[1];
};

static char upBuf[512];

// 放 .data 确保上电即初始化;结构整体 volatile 语义由写入点保证
static RTTControlBlock rttCB;

static RTTControlBlock *cb() { return &rttCB; }

void setup() {
  Serial.begin(115200);
  memset(&rttCB, 0, sizeof(rttCB));
  strcpy(rttCB.id, "SEGGER RTT");      // 剩余 5 字节为 0 ✓
  rttCB.maxUp = 1;
  rttCB.maxDown = 1;
  rttCB.up[0].name = "Terminal";
  rttCB.up[0].buf = upBuf;
  rttCB.up[0].size = sizeof(upBuf);
  rttCB.up[0].wrOff = 0;
  rttCB.up[0].rdOff = 0;
  rttCB.up[0].flags = 0;               // 0 = 无阻塞覆盖?probe-rs 默认 2( NO_BLOCK_SKIP)
  rttCB.down[0].name = "Terminal";
  rttCB.down[0].buf = nullptr;
  rttCB.down[0].size = 0;
}

static unsigned written = 0;
static uint32_t counter = 0;

void loop() {
  static uint32_t last = 0;
  if (millis() - last < 200) return;
  last = millis();

  char msg[64];
  int n = snprintf(msg, sizeof(msg), "Hello from RTT #%u\n", (unsigned)counter++);

  RTTControlBlock *cbp = cb();
  unsigned wr = cbp->up[0].wrOff;
  for (int i = 0; i < n; i++) {
    upBuf[wr] = msg[i];
    wr = (wr + 1) % sizeof(upBuf);
  }
  cbp->up[0].wrOff = wr;               // volatile 语义:JTAG 侧直读 RAM
  written += n;
  (void)written;
}
