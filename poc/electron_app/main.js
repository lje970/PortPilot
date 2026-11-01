// POC-C:Electron + node-serialport 最小串口收流工程。
// 主进程 I/O 线程读流,33ms 批量 IPC 推给渲染进程;渲染进程节流渲染并回报 fps。
// 遵循 poc/probe_protocol.md v1。(POC 阶段用纯 JS,产品化再转 TS)

const { app, BrowserWindow, ipcMain } = require('electron');
const { SerialPort } = require('serialport');
const fs = require('fs');

// ---- CLI 解析(--port --baud --seconds --out) ----
const args = {};
for (let i = 2; i < process.argv.length; i++) {
  if (process.argv[i].startsWith('--')) args[process.argv[i].slice(2)] = process.argv[i + 1];
}
const PORT = args.port || 'COM1';
const BAUD = parseInt(args.baud || '921600', 10);
const SECONDS = parseFloat(args.seconds || '30');
const OUT = args.out || 'probe_results.jsonl';

let win = null;
let port = null;
let quitFlag = false;
let reconnects = 0;

// ---- 探针状态(主进程) ----
let rxBytes = 0;
let buf = [];
let bufLen = 0;
let arrivals = [];          // { oldest, newest } (performance.now ms)
let lastReadTs = null;
let iarSamples = [];
let latSamples = [];        // 渲染回报的到达→上屏延迟
let rendersThisSec = 0;
let batchId = 0;
let t0 = null;

function writeProbe(line) {
  fs.appendFileSync(OUT, JSON.stringify(line) + '\n');
}

function openPort(retry = false) {
  port = new SerialPort({ path: PORT, baudRate: BAUD, autoOpen: false });
  port.on('data', (chunk) => {
    const now = performance.now();
    if (lastReadTs !== null) iarSamples.push(now - lastReadTs);
    lastReadTs = now;
    buf.push(chunk);
    bufLen += chunk.length;
    arrivals.push({ oldest: now, newest: now });
  });
  port.on('close', () => {
    if (!quitFlag) {
      console.log(`[electron] 串口关闭,500ms 后重连`);
      reconnects++;
      setTimeout(() => openPort(true), 500);
    }
  });
  port.on('error', (err) => console.error('[electron] port error:', err.message));
  port.open((err) => {
    if (err) {
      console.error(`[electron] 打开 ${PORT} 失败: ${err.message}`);
      app.exit(2);
    } else {
      console.log(`[electron] 已打开 ${PORT} @ ${BAUD}`);
    }
  });
}

function start() {
  win = new BrowserWindow({
    width: 560, height: 440,
    webPreferences: { nodeIntegration: true, contextIsolation: false },
  });
  win.loadFile('index.html');

  openPort();
  t0 = performance.now();

  // 33ms 批量推流(UI 节流 30fps)
  const FLUSH_MS = 33;
  setInterval(() => {
    if (!bufLen) return;
    const data = Buffer.concat(buf);
    rxBytes += data.length;
    const arrNewest = arrivals[arrivals.length - 1].newest;
    iarWindow.push(...iarSamples);
    buf = []; bufLen = 0; arrivals = []; iarSamples = [];
    const id = ++batchId;
    win.webContents.send('batch', {
      id, bytes: data.length, preview: data.subarray(0, 64).toString('hex'),
      arrNewest, sentAt: performance.now(),
    });
  }, FLUSH_MS);

  // 每秒探针行
  let sec = 0;
  let iarWindow = [];
  setInterval(() => {
    sec++;
    const iarSorted = [...iarWindow].sort((a, b) => a - b);
    const p95 = (a) => (a.length ? a[Math.max(0, Math.floor(a.length * 0.95) - 1)] : 0);
    const latP95 = p95(latSamples);
    writeProbe({
      t: sec, rx_bytes: rxBytes,
      ui_fps: rendersThisSec,
      latency_p95_ms: Math.round(latP95 * 100) / 100,
      inter_arrival_p95_ms: Math.round(p95(iarSorted) * 100) / 100,
      reconnects,
    });
    win.setTitle(`POC-C Electron · ${sec}s · ${rendersThisSec} fps`);
    rendersThisSec = 0; latSamples = []; iarWindow = [];
  }, 1000);

  // 收尾退出(多排水 5s,吸收链路在途数据,四栈同口径)
  setTimeout(() => {
    quitFlag = true;
    if (port && port.isOpen) port.close();
    app.quit();
  }, (SECONDS + 5) * 1000);
}

ipcMain.on('rendered', (_e, { id, sentAt, arrNewest }) => {
  const now = performance.now();
  latSamples.push(now - arrNewest);
  void id; void sentAt;
});
ipcMain.on('fps', (_e, { renders }) => { rendersThisSec = renders; });

app.whenReady().then(start);
app.on('window-all-closed', () => { if (!quitFlag) app.quit(); });
