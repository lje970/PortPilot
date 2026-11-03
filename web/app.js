// PortPilot Web · Web Serial API 轻量版(引流/轻调试)
// 功能:连接/收发(文本+HEX)/时间戳/HEX 视图/JustFloat 示波器
// 数据仅在本地浏览器与串口间流动,不经任何服务器。
'use strict';

const $ = (id) => document.getElementById(id);
const TAIL = new Uint8Array([0x00, 0x00, 0x80, 0x7f]); // JustFloat 帧尾
const textEnc = new TextEncoder();
const gbkDec = new TextDecoder('gbk');
const utf8Dec = new TextDecoder('utf-8');

// ---------- 状态 ----------
let port = null;
let reader = null;
let writer = null;
let keepReading = false;
let connected = false;

let rxBytes = 0;
let txBytes = 0;
let pending = [];      // JustFloat 解析缓冲
let scopeFrames = [];  // {t, ch[]}
let lineBuf = [];      // 跨块行组装
let paused = false;
let pausedText = [];   // 暂停期间的行

// ---------- 兼容检查 ----------
if (!('serial' in navigator)) {
  $('unsupported').classList.remove('hidden');
}

// ---------- 工具 ----------
function nowTs() { return new Date().toISOString().slice(11, 23); }
function esc(s) { return s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c])); }
function appendLine(kind, ts, text) {
  const div = document.createElement('span');
  div.className = 'ln';
  if ($('tsChk').checked && ts) {
    const t = document.createElement('span');
    t.className = 'ts'; t.textContent = ts;
    div.appendChild(t);
  }
  const s = document.createElement('span');
  s.className = kind; s.textContent = text;
  div.appendChild(s);
  const term = $('term');
  term.appendChild(div);
  while (term.childElementCount > 3000) term.removeChild(term.firstChild);
  if ($('scrollChk').checked && !paused) term.scrollTop = term.scrollHeight;
}
function sysLine(text) { appendLine('sys', nowTs(), text); }

function decodeBytes(raw) {
  return $('encSel').value === 'gbk' ? gbkDec.decode(raw) : utf8Dec.decode(raw, { stream: true });
}

// ---------- 连接 ----------
$('connectBtn').onclick = async () => {
  try {
    if (!connected) {
      if (!('serial' in navigator)) return alert('浏览器不支持 Web Serial');
      port = await navigator.serial.requestPort();
      const baud = Number($('baudSel').value);
      await port.open({ baudRate: baud });
      connected = true;
      $('dot').className = 'dot on';
      $('connectBtn').textContent = '断开';
      $('connectBtn').classList.add('on');
      sysLine(`── 已连接 @ ${baud} ──`);
      readLoop();
    } else {
      keepReading = false;
      if (reader) await reader.cancel().catch(() => {});
      await disconnectPort();
      sysLine('── 已断开 ──');
    }
  } catch (e) {
    sysLine(`!! ${e.message || e}`);
  }
};

async function disconnectPort() {
  connected = false;
  $('dot').className = 'dot off';
  $('connectBtn').textContent = '连接';
  $('connectBtn').classList.remove('on');
  try { if (port) await port.close(); } catch {}
  port = null; writer = null;
}

async function readLoop() {
  while (connected && port && port.readable) {
    reader = port.readable.getReader();
    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done || !keepReadingAndConnected()) break;
        if (value && value.length) handleBytes(value);
      }
    } catch (e) {
      if (keepReadingAndConnected()) sysLine(`!! 读错误: ${e.message}`);
    } finally {
      reader.releaseLock();
    }
    if (!keepReadingAndConnected()) break;
    await new Promise((r) => setTimeout(r, 300)); // 断线重连退避
  }
}
function keepReadingAndConnected() { return keepReading && connected; }

// ---------- 数据处理 ----------
function handleBytes(value) {
  rxBytes += value.length;
  // JustFloat 抽头(示波器启用时)
  if ($('scopeOn').checked) feedScope(value);
  // 行组装 + 文本渲染
  const text = decodeBytes(value);
  lineBuf.push(text);
  const all = lineBuf.join('');
  lineBuf.length = 0;
  const parts = all.split('\n');
  lineBuf.push(parts.pop()); // 末段可能不完整
  const ts = nowTs();
  for (const p of parts) {
    const line = p.replace(/\r/g, '');
    if (line.length) {
      renderLine('rx', ts, line);
    }
  }
  $('statTxt').textContent = `RX ${rxBytes} · TX ${txBytes}`;
}

// ---------- 发送 ----------
async function doSend() {
  const data = $('sendInput').value;
  if (!data || !connected) return;
  let bytes;
  if ($('sendHexChk').checked) {
    const c = data.replace(/[^0-9a-fA-F]/g, '');
    if (c.length % 2) return alert('HEX 位数必须为偶数');
    bytes = new Uint8Array((c.match(/../g) || []).map((h) => parseInt(h, 16)));
  } else {
    const nl = { cr: '\r', lf: '\n', crlf: '\r\n', none: '' }[$('nlSel').value];
    bytes = textEnc.encode(data + nl);
  }
  if (!port || !port.writable) return sysLine('!! 未连接');
  writer = port.writable.getWriter();
  await writer.write(bytes);
  writer.releaseLock();
  txBytes += bytes.length;
  const ts = nowTs();
  const disp = $('sendHexChk').checked
    ? '(HEX) ' + data.replace(/[^0-9a-fA-F]/g, '').toUpperCase()
    : data + { cr: '\\r', lf: '\\n', crlf: '\\r\\n', none: '' }[$('nlSel').value];
  renderLine('tx', ts, '→ ' + disp);
  $('statTxt').textContent = `RX ${rxBytes} · TX ${txBytes}`;
  history.unshift(data);
  if (history.length > 100) history.pop();
  localStorage.setItem('ppw_history', JSON.stringify(history));
  histIdx = -1;
  $('sendInput').value = '';
  $('sendInput').focus();
}
const history = JSON.parse(localStorage.getItem('ppw_history') || '[]');
let histIdx = -1;
$('sendBtn').onclick = doSend;
$('sendInput').addEventListener('keydown', (e) => {
  if (e.key === 'Enter') { e.preventDefault(); doSend(); return; }
  if (e.key === 'ArrowUp' && history.length) {
    e.preventDefault();
    histIdx = Math.min(histIdx + 1, history.length - 1);
    $('sendInput').value = history[histIdx];
  } else if (e.key === 'ArrowDown') {
    e.preventDefault();
    histIdx = Math.max(histIdx - 1, -1);
    $('sendInput').value = histIdx >= 0 ? history[histIdx] : '';
  }
});

// ---------- 工具栏 ----------
$('hexChk').onchange = () => { /* 简化:HEX 切换仅影响后续新数据的展示习惯,网页版以文本流为主 */ };
$('pauseBtn').onclick = () => {
  paused = !paused;
  $('pauseBtn').textContent = paused ? '▶' : '⏸';
  if (!paused) {
    for (const [k, t, s] of pausedText.splice(0)) renderLine(k, t, s);
    term.scrollTop = term.scrollHeight;
  }
};
$('clearBtn').onclick = () => { $('term').innerHTML = ''; $('lineStat').textContent = '0 行'; };
function renderLine(kind, ts, text) {
  if (paused) { pausedText.push([kind, ts, text]); if (pausedText.length > 500) pausedText.shift(); return; }
  appendLine(kind, ts, text);
}

// ---------- 示波器(JustFloat) ----------
const scopeCanvas = $('scopeCanvas');
let scopeDrawQueued = false;
const CH_COLORS = ['#4cc2ff', '#ffd166', '#7ce38b', '#ff8fa3'];

function feedScope(value) {
  pending.push(...value);
  const chN = Number($('scopeCh').value);
  const frameLen = chN * 4 + 4;
  let guard = 0;
  for (;;) {
    const t = findSeq(pending, TAIL);
    if (t === -1 || t < frameLen - 4) break;
    const start = t + 4 - frameLen;
    const ch = [];
    for (let c = 0; c < chN; c++) {
      ch.push(new DataView(pending.buffer, pending.byteOffset + start + c * 4, 4).getFloat32(0, true));
    }
    scopeFrames.push({ t: performance.now(), ch });
    pending.splice(0, t + 4);
    if (++guard > 4096) { pending.length = 0; break; }
  }
  while (scopeFrames.length > 600) scopeFrames.shift();
  if (!$('scopePanel').classList.contains('hidden')) queueScopeDraw();
}
function findSeq(hay, needle) {
  outer: for (let i = 0; i + needle.length <= hay.length; i++) {
    for (let j = 0; j < needle.length; j++) if (hay[i + j] !== needle[j]) continue outer;
    return i;
  }
  return -1;
}
function queueScopeDraw() {
  if (scopeDrawQueued) return;
  scopeDrawQueued = true;
  requestAnimationFrame(() => { scopeDrawQueued = false; drawScope(); });
}
function drawScope() {
  const c = scopeCanvas;
  const dpr = window.devicePixelRatio || 1;
  if (c.width !== c.clientWidth * dpr) { c.width = c.clientWidth * dpr; c.height = c.clientHeight * dpr; }
  const ctx = c.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  const W = c.clientWidth, H = c.clientHeight;
  ctx.clearRect(0, 0, W, H);
  ctx.strokeStyle = '#22262f';
  ctx.beginPath();
  for (let x = 0; x <= W; x += W / 10) { ctx.moveTo(x, 0); ctx.lineTo(x, H); }
  for (let y = 0; y <= H; y += H / 6) { ctx.moveTo(0, y); ctx.lineTo(W, y); }
  ctx.stroke();
  if (!scopeFrames.length) {
    ctx.fillStyle = '#5a606a'; ctx.font = '12px Segoe UI';
    ctx.fillText('等待 JustFloat 帧…(下位机按 [f32×N][00 00 80 7F] 发送)', 12, 20);
    return;
  }
  let mn = Infinity, mx = -Infinity;
  for (const f of scopeFrames) for (const v of f.ch) { if (v < mn) mn = v; if (v > mx) mx = v; }
  if (mn === mx) { mn -= 1; mx += 1; }
  const pad = (mx - mn) * 0.1; mn -= pad; mx += pad;
  const chN = scopeFrames[scopeFrames.length - 1].ch.length;
  for (let cI = 0; cI < chN; cI++) {
    ctx.strokeStyle = CH_COLORS[cI % CH_COLORS.length];
    ctx.lineWidth = 1.4;
    ctx.beginPath();
    const n = scopeFrames.length;
    for (let i = 0; i < n; i++) {
      const v = scopeFrames[i].ch[cI];
      if (v === undefined) continue;
      const x = (i / (n - 1)) * (W - 8) + 4;
      const y = H - ((v - mn) / (mx - mn)) * (H - 16) - 8;
      i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
    }
    ctx.stroke();
  }
  ctx.font = '11px Consolas';
  for (let cI = 0; cI < chN; cI++) {
    const last = scopeFrames[scopeFrames.length - 1].ch[cI];
    ctx.fillStyle = CH_COLORS[cI % CH_COLORS.length];
    ctx.fillText(`CH${cI + 1} ${last !== undefined ? last.toFixed(2) : '-'}`, 10 + cI * 90, 12);
  }
  $('scopeInfo').textContent = `${scopeFrames.length} 帧 · Y[${mn.toFixed(1)}, ${mx.toFixed(1)}]`;
}
$('scopeOn').onchange = () => {
  $('scopePanel').classList.toggle('hidden', !$('scopeOn').checked);
  if ($('scopeOn').checked) {
    pending.length = 0; scopeFrames.length = 0;
    queueScopeDraw();
  }
};
$('scopeCh').onchange = () => { pending.length = 0; scopeFrames.length = 0; };
$('scopeClear').onclick = () => { scopeFrames.length = 0; drawScope(); };
window.addEventListener('resize', () => { if (!$('scopePanel').classList.contains('hidden')) drawScope(); });

// ---------- 就绪 ----------
sysLine('── PortPilot Web 就绪:点击"连接"并在弹窗中选择串口设备 ──');
