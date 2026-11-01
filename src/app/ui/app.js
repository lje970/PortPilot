// PortPilot 前端逻辑(MVP 0.1.0)
// 三段式:连接栏 / 终端区(文本+HEX 双模式、时间戳、暂停、清屏)/ 发送面板(历史+快捷按钮)
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const term = $('term');
const MAX_LINES = 4000;          // 终端缓冲上限(超出丢弃最旧;虚拟滚动 P1 迭代)
let textLines = [];              // {ts, s, kind: 'rx'|'tx'|'sys'}
let hexLines = [];               // {ts, s}
let paused = false;
let pausedQueue = [];            // 暂停期间的待渲染帧
let droppedFrames = 0;
let connected = false;

// ---------- 终端渲染 ----------
function fmtTs(ts) {
  if (!$('tsChk').checked || !ts) return null;
  return ts;
}
function appendLine(obj) {
  const div = document.createElement('span');
  div.className = 'ln';
  const ts = fmtTs(obj.ts);
  if (ts) {
    const t = document.createElement('span');
    t.className = 'ts'; t.textContent = ts;
    div.appendChild(t);
  }
  const s = document.createElement('span');
  s.className = obj.kind || 'rx';
  s.textContent = obj.s;
  div.appendChild(s);
  term.appendChild(div);
}
function trimTerm() {
  while (term.childElementCount > MAX_LINES) term.removeChild(term.firstChild);
}
function autoScroll() {
  if ($('scrollChk').checked && !paused) term.scrollTop = term.scrollHeight;
}
function renderStats() {
  $('lineStat').textContent = `${term.childElementCount} 行${droppedFrames ? ` · 暂停丢帧 ${droppedFrames}` : ''}`;
}

// ---------- 连接 ----------
async function refreshPorts() {
  const ports = await invoke('list_ports');
  const sel = $('portSel');
  sel.innerHTML = '';
  for (const p of ports) {
    const o = document.createElement('option');
    o.value = p.name;
    o.textContent = p.usb ? `${p.name}  (${p.usb})` : p.name;
    sel.appendChild(o);
  }
  if (ports.some(p => p.name === 'COM9')) sel.value = 'COM9';
}
$('refreshBtn').onclick = refreshPorts;

async function setConnectedUI(on, info) {
  connected = on;
  $('dot').className = 'dot ' + (on ? 'on' : 'off');
  $('connectBtn').textContent = on ? '断开' : '连接';
  $('connectBtn').classList.toggle('on', on);
  $('connInfo').textContent = info || '';
}

$('connectBtn').onclick = async () => {
  try {
    if (!connected) {
      const port = $('portSel').value;
      const baud = Number($('customBaud').value) || Number($('baudSel').value);
      if (!port) return alert('未检测到串口,请先刷新');
      const msg = await invoke('connect', { port, baud });
      await setConnectedUI(true, msg);
      appendLine({ kind: 'sys', s: `── 已连接 ${port} @ ${baud} ──` });
    } else {
      const msg = await invoke('disconnect');
      await setConnectedUI(false, '');
      appendLine({ kind: 'sys', s: `── ${msg} ──` });
    }
    autoScroll();
  } catch (e) {
    appendLine({ kind: 'sys', s: `!! ${e}` });
    autoScroll();
  }
};

$('dtrBtn').onclick = async () => {
  const on = !$('dtrBtn').classList.contains('on');
  $('dtrBtn').classList.toggle('on', on);
  try { await invoke('set_dtr', { on }); } catch (e) { appendLine({ kind: 'sys', s: `!! ${e}` }); }
};
$('rtsBtn').onclick = async () => {
  const on = !$('rtsBtn').classList.contains('on');
  $('rtsBtn').classList.toggle('on', on);
  try { await invoke('set_rts', { on }); } catch (e) { appendLine({ kind: 'sys', s: `!! ${e}` }); }
};

// ---------- 接收 ----------
listen('rx', (e) => {
  const d = e.payload;
  if (paused) {
    pausedQueue.push(d);
    if (pausedQueue.length > 300) { pausedQueue.shift(); droppedFrames++; }
    return;
  }
  consume(d);
});
function consume(d) {
  for (const l of d.lines) {
    textLines.push({ ts: l.ts, s: l.s, kind: 'rx' });
    appendLine({ ts: l.ts, s: l.s, kind: 'rx' });
  }
  if (d.hex) {
    hexLines.push({ ts: new Date().toISOString().slice(11, 23), s: d.hex, kind: 'hex' });
    if ($('hexChk').checked) appendLine({ ts: null, s: d.hex, kind: 'hex' });
  }
  trimTerm();
  renderStats();
  autoScroll();
}

// ---------- 工具栏 ----------
$('hexChk').onchange = () => {
  term.innerHTML = '';
  const src = $('hexChk').checked ? hexLines : textLines;
  for (const l of src) appendLine(l);
  trimTerm(); autoScroll();
};
$('pauseBtn').onclick = () => {
  paused = !paused;
  $('pauseBtn').textContent = paused ? '▶ 继续' : '⏸ 暂停';
  $('pauseBtn').classList.toggle('on', paused);
  if (!paused) {
    for (const d of pausedQueue) consume(d);
    pausedQueue = [];
    renderStats(); autoScroll();
  }
};
$('clearBtn').onclick = () => {
  term.innerHTML = '';
  textLines = []; hexLines = []; droppedFrames = 0;
  renderStats();
};

// 每秒状态栏
setInterval(async () => {
  try {
    const st = await invoke('stats');
    const rxRate = st.rx_bytes - (window._lastRx || 0);
    window._lastRx = st.rx_bytes;
    $('statTxt').textContent =
      `RX ${st.rx_bytes} (${rxRate}/s) · TX ${st.tx_bytes} · 重连 ${st.reconnects}` +
      (st.backpressure ? ` · 背压 ${st.backpressure}` : '');
    if (st.connected !== connected) await setConnectedUI(st.connected, st.connected ? '已连接' : '');
  } catch {}
}, 1000);

// ---------- 发送 ----------
const history = JSON.parse(localStorage.getItem('pp_history') || '[]');
let histIdx = -1;

function nlDisp(mode) { return mode === 'cr' ? '\\r' : mode === 'lf' ? '\\n' : mode === 'crlf' ? '\\r\\n' : ''; }

async function doSend() {
  const data = $('sendInput').value;
  if (!data) return;
  try {
    await invoke('send', {
      data,
      hexMode: $('sendHexChk').checked,
      newline: $('nlSel').value,
    });
    const disp = $('sendHexChk').checked
      ? '(HEX) ' + data.replace(/[^0-9a-fA-F]/g, '').toUpperCase()
      : data + nlDisp($('nlSel').value);
    const ts = new Date().toISOString().slice(11, 23);
    textLines.push({ ts, s: '→ ' + disp, kind: 'tx' });
    appendLine({ ts, s: '→ ' + disp, kind: 'tx' });
    trimTerm(); autoScroll();
    history.unshift(data);
    if (history.length > 100) history.pop();
    localStorage.setItem('pp_history', JSON.stringify(history));
    histIdx = -1;
    $('sendInput').value = '';
    $('sendInput').focus();
  } catch (e) {
    appendLine({ kind: 'sys', s: `!! 发送失败: ${e}` });
    autoScroll();
  }
}
$('sendBtn').onclick = doSend;
$('sendInput').addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') { ev.preventDefault(); doSend(); return; }
  if (ev.key === 'ArrowUp' && history.length) {
    ev.preventDefault();
    histIdx = Math.min(histIdx + 1, history.length - 1);
    $('sendInput').value = history[histIdx];
  } else if (ev.key === 'ArrowDown') {
    ev.preventDefault();
    histIdx = Math.max(histIdx - 1, -1);
    $('sendInput').value = histIdx >= 0 ? history[histIdx] : '';
  }
});

// ---------- 快捷发送按钮(8 个,localStorage 持久化) ----------
function loadQuick() {
  return JSON.parse(localStorage.getItem('pp_quick') || 'null') ||
    Array.from({ length: 8 }, (_, i) => ({ label: `快捷${i + 1}`, data: '', hex: false, nl: 'none' }));
}
function renderQuick() {
  const row = $('quickRow');
  row.innerHTML = '';
  loadQuick().forEach((q, i) => {
    const b = document.createElement('button');
    b.textContent = q.label;
    b.onclick = async () => {
      if (!q.data) return editQuick(i);
      try {
        await invoke('send', { data: q.data, hexMode: q.hex, newline: q.nl });
        const ts = new Date().toISOString().slice(11, 23);
        appendLine({ ts, s: `→ [${q.label}] ${q.hex ? '(HEX) ' + q.data : q.data}`, kind: 'tx' });
        trimTerm(); autoScroll();
      } catch (e) { appendLine({ kind: 'sys', s: `!! ${e}` }); }
    };
    b.ondblclick = () => editQuick(i);
    row.appendChild(b);
  });
}
function editQuick(i) {
  const arr = loadQuick();
  const label = prompt('按钮名:', arr[i].label);
  if (label === null) return;
  const data = prompt('发送内容(HEX 模式下填十六进制):', arr[i].data);
  if (data === null) return;
  const hex = confirm('以 HEX 模式发送?(确定=HEX,取消=文本)');
  const nl = prompt('追加新行:cr / lf / crlf / none', arr[i].nl || 'none') || 'none';
  arr[i] = { label: label || `快捷${i + 1}`, data, hex, nl };
  localStorage.setItem('pp_quick', JSON.stringify(arr));
  renderQuick();
}
renderQuick();

// ---------- 启动 ----------
refreshPorts();
appendLine({ kind: 'sys', s: '── PortPilot 0.1.0 就绪 ──' });
