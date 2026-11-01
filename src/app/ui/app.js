// PortPilot 前端 0.1.0 · 虚拟滚动版
// 三段式:连接栏(档案)/ 终端区(虚拟滚动百万行 + 过滤高亮 + HEX/时间戳/暂停)/ 发送面板
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const LH = 18;                     // 行高,与 --lh 一致
const MAX_STORED = 1000000;        // 每视图最大存储行数(超出丢弃最旧并计数)
const TRUNC = 4096;                // 单行显示截断长度(完整数据在日志 bin 里)

// ---------- 虚拟滚动日志 ----------
class VirtualLog {
  constructor(viewportId) {
    this.viewport = $(viewportId);
    this.inner = this.viewport.firstElementChild;
    this.lines = [];      // {ts, s, kind, hidden}
    this.visible = [];    // 通过过滤的引用
    this.max = MAX_STORED;
    this.dropped = 0;
    this.filter = null;   // {mode, testRe, markRe}
    this.dirty = true;
    this.canvas = document.createElement('canvas').getContext('2d');
    this.canvas.font = '12.5px Consolas';
    this.pool = [];
    this.viewport.addEventListener('scroll', () => this.render());
    window.addEventListener('resize', () => this.render(true));
  }
  hide(l) {
    if (!this.filter) return false;
    const hit = this.filter.testRe.test(l.s);
    this.filter.testRe.lastIndex = 0;
    return this.filter.mode === 'include' ? !hit : hit;
  }
  push(l) {
    l.hidden = this.hide(l);
    if (!l.hidden) this.visible.push(l);
    this.lines.push(l);
    if (this.lines.length > this.max) this.trim();
    this.dirty = true;
  }
  trim() {
    const over = this.lines.length - this.max;
    if (over <= 0) return;
    const cut = Math.min(over, 10000);
    let k = 0;
    for (let i = 0; i < cut; i++) if (!this.lines[i].hidden) k++;
    this.visible.splice(0, k);
    this.lines.splice(0, cut);
    this.dropped += cut;
    this.viewport.scrollTop = Math.max(0, this.viewport.scrollTop - k * LH);
  }
  applyFilter(f) {
    this.filter = f;
    this.visible = this.lines.filter((l) => !this.hide(l));
    this.dirty = true;
  }
  clear() {
    this.lines = []; this.visible = []; this.dropped = 0;
    this.dirty = true;
  }
  count() {
    return { visible: this.visible.length, stored: this.lines.length, dropped: this.dropped };
  }
  wasAtBottom() {
    return this.viewport.scrollTop + this.viewport.clientHeight >= this.inner.offsetHeight - LH * 2;
  }
  scrollBottom() {
    this.viewport.scrollTop = this.inner.offsetHeight;
  }
  render(force) {
    if (!this.dirty && !force) return;
    this.dirty = false;
    const vh = this.viewport.clientHeight;
    const total = this.visible.length;
    this.inner.style.height = total * LH + 'px';
    // 视窗裁剪:仅渲染可见 ±10 行
    let first = Math.floor(this.viewport.scrollTop / LH) - 5;
    let count = Math.ceil(vh / LH) + 10;
    first = Math.max(0, Math.min(first, Math.max(0, total - count)));
    const end = Math.min(total, first + count);
    // 复用 DOM 池
    for (let i = 0; i < this.pool.length; i++) {
      this.pool[i].style.display = 'none';
    }
    let maxW = 0;
    for (let i = first; i < end; i++) {
      const l = this.visible[i];
      let el = this.pool[i - first];
      if (!el) {
        el = document.createElement('span');
        el.className = 'ln';
        this.pool[i - first] = el;
        this.inner.appendChild(el);
      }
      el.style.display = 'block';
      el.style.top = i * LH + 'px';
      const body = l.s.length > TRUNC ? l.s.slice(0, TRUNC) + '…(截断)' : l.s;
      let html = '';
      if ($('tsChk').checked && l.ts) html += `<span class="ts">${esc(l.ts)}</span>`;
      html += `<span class="${l.kind}">${highlight(body, this.filter)}</span>`;
      el.innerHTML = html;
      maxW = Math.max(maxW, this.canvas.measureText(body).width + (l.ts ? 90 : 0) + 24);
    }
    this.inner.style.width = Math.max(this.viewport.clientWidth, maxW) + 'px';
  }
}

function esc(s) { return s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' }[c])); }
function highlight(body, filter) {
  const e = esc(body);
  if (filter && filter.mode === 'include' && filter.markRe) {
    return e.replace(filter.markRe, (m) => `<mark>${m}</mark>`);
  }
  return e;
}
function buildFilter(text, mode) {
  const t = text.trim();
  if (!t) return null;
  if (t.startsWith('re:')) {
    let re;
    try { re = new RegExp(t.slice(3), 'i'); } catch { return null; }
    return {
      mode, testRe: re,
      markRe: (() => { try { return new RegExp(t.slice(3), 'gi'); } catch { return null; } })(),
    };
  }
  const escd = t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return {
    mode, testRe: new RegExp(escd, 'i'),
    markRe: new RegExp(escd, 'gi'),
  };
}

// ---------- 两个视图 ----------
const vText = new VirtualLog('termText');
const vHex = new VirtualLog('termHex');
const active = () => ($('hexChk').checked ? vHex : vText);

function renderActive(force) {
  vHex.render(force);
  vText.render(force);
  updateLineStat();
}
function updateLineStat() {
  const c = active().count();
  $('lineStat').textContent =
    `${c.visible} 行 · 存 ${c.stored}${c.dropped ? ` · 丢旧 ${c.dropped}` : ''}`;
}

// ---------- 接收 ----------
listen('rx', (e) => {
  const d = e.payload;
  const stick = vText.wasAtBottom();
  for (const l of d.lines) vText.push({ ts: l.ts, s: l.s, kind: 'rx' });
  if (d.hex) {
    const ts = new Date().toISOString().slice(11, 23);
    vHex.push({ ts, s: d.hex, kind: 'hex' });
  }
  renderActive();
  if ($('scrollChk').checked && stick) {
    const v = active();
    v.render(true);
    v.scrollBottom();
  }
});

// ---------- 工具栏 ----------
let paused = false;
$('hexChk').onchange = () => {
  $('termText').classList.toggle('hidden', $('hexChk').checked);
  $('termHex').classList.toggle('hidden', !$('hexChk').checked);
  renderActive(true);
  if ($('scrollChk').checked) active().scrollBottom();
};
$('pauseBtn').onclick = () => {
  paused = !paused;
  $('pauseBtn').textContent = paused ? '▶' : '⏸';
  $('pauseBtn').classList.toggle('on', paused);
  renderActive(true);
};
$('clearBtn').onclick = () => {
  vText.clear(); vHex.clear(); renderActive(true);
};
let filterTimer = null;
$('filterInput').addEventListener('input', () => {
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => {
    vText.applyFilter(buildFilter($('filterInput').value, $('filterMode').value));
    renderActive(true);
  }, 200);
});
$('filterMode').onchange = () => {
  vText.applyFilter(buildFilter($('filterInput').value, $('filterMode').value));
  renderActive(true);
};
$('encSel').onchange = async () => {
  try { await invoke('set_encoding', { encoding: $('encSel').value }); } catch (e) { alert(e); }
};

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
  if (ports.some((p) => p.name === 'COM9')) sel.value = 'COM9';
}
$('refreshBtn').onclick = refreshPorts;

async function setConnectedUI(on, info) {
  $('dot').className = 'dot ' + (on ? 'on' : 'off');
  $('connectBtn').textContent = on ? '断开' : '连接';
  $('connectBtn').classList.toggle('on', on);
  $('connInfo').textContent = info || '';
  $('connInfo').title = info || '';
}

$('connectBtn').onclick = async () => {
  try {
    if (!$('connectBtn').classList.contains('on')) {
      const port = $('portSel').value;
      const baud = Number($('customBaud').value) || Number($('baudSel').value);
      if (!port) return alert('未检测到串口,请先刷新');
      const msg = await invoke('connect', { port, baud, encoding: $('encSel').value });
      await setConnectedUI(true, msg);
      vText.push({ ts: nowTs(), s: `── 已连接 ${port} @ ${baud} ──`, kind: 'sys' });
    } else {
      const msg = await invoke('disconnect');
      await setConnectedUI(false, '');
      vText.push({ ts: nowTs(), s: `── ${msg} ──`, kind: 'sys' });
    }
    renderActive(true); autoScroll();
  } catch (e) {
    vText.push({ ts: nowTs(), s: `!! ${e}`, kind: 'sys' });
    renderActive(true); autoScroll();
  }
};

$('dtrBtn').onclick = async () => {
  const on = !$('dtrBtn').classList.contains('on');
  $('dtrBtn').classList.toggle('on', on);
  try { await invoke('set_dtr', { on }); } catch (e) { vText.push({ ts: nowTs(), s: `!! ${e}`, kind: 'sys' }); renderActive(true); }
};
$('rtsBtn').onclick = async () => {
  const on = !$('rtsBtn').classList.contains('on');
  $('rtsBtn').classList.toggle('on', on);
  try { await invoke('set_rts', { on }); } catch (e) { vText.push({ ts: nowTs(), s: `!! ${e}`, kind: 'sys' }); renderActive(true); }
};

// 每秒状态栏
setInterval(async () => {
  try {
    const st = await invoke('stats');
    const rate = st.rx_bytes - (window._lastRx || 0);
    window._lastRx = st.rx_bytes;
    $('statTxt').textContent =
      `RX ${st.rx_bytes} (${rate}/s) · TX ${st.tx_bytes} · 重连 ${st.reconnects}` +
      (st.backpressure ? ` · 背压 ${st.backpressure}` : '');
    const on = st.connected;
    if (on !== $('connectBtn').classList.contains('on')) {
      await setConnectedUI(on, on ? '已连接' : '');
    }
  } catch {}
}, 1000);

// ---------- 连接档案 ----------
function loadProfiles() { return JSON.parse(localStorage.getItem('pp_profiles') || '[]'); }
function renderProfiles() {
  const sel = $('profileSel');
  sel.innerHTML = '<option value="">档案…</option>';
  for (const p of loadProfiles()) {
    const o = document.createElement('option');
    o.value = p.name;
    o.textContent = `${p.name} (${p.port}@${p.baud})`;
    sel.appendChild(o);
  }
}
$('profileSel').onchange = () => {
  const p = loadProfiles().find((x) => x.name === $('profileSel').value);
  if (!p) return;
  $('customBaud').value = '';
  refreshPorts().then(() => {
    if ([...$('portSel').options].some((o) => o.value === p.port)) $('portSel').value = p.port;
    $('baudSel').value = String(p.baud);
    if (![...$('baudSel').options].some((o) => o.value === String(p.baud))) $('customBaud').value = p.baud;
    $('encSel').value = p.enc || 'utf8';
    $('encSel').onchange();
  });
};
$('profileSave').onclick = () => {
  const name = prompt('档案名称:', `档案${loadProfiles().length + 1}`);
  if (!name) return;
  const arr = loadProfiles().filter((p) => p.name !== name);
  arr.push({
    name, port: $('portSel').value,
    baud: Number($('customBaud').value) || Number($('baudSel').value),
    enc: $('encSel').value,
  });
  localStorage.setItem('pp_profiles', JSON.stringify(arr));
  renderProfiles();
  $('profileSel').value = name;
};
$('profileDel').onclick = () => {
  const name = $('profileSel').value;
  if (!name) return;
  localStorage.setItem('pp_profiles', JSON.stringify(loadProfiles().filter((p) => p.name !== name)));
  renderProfiles();
};
renderProfiles();

// ---------- 发送 ----------
const history = JSON.parse(localStorage.getItem('pp_history') || '[]');
let histIdx = -1;
function nowTs() { return new Date().toISOString().slice(11, 23); }

async function doSend() {
  const data = $('sendInput').value;
  if (!data) return;
  try {
    await invoke('send', { data, hexMode: $('sendHexChk').checked, newline: $('nlSel').value });
    const disp = $('sendHexChk').checked
      ? '(HEX) ' + data.replace(/[^0-9a-fA-F]/g, '').toUpperCase()
      : data + { cr: '\\r', lf: '\\n', crlf: '\\r\\n', none: '' }[$('nlSel').value];
    vText.push({ ts: nowTs(), s: '→ ' + disp, kind: 'tx' });
    renderActive(true); autoScroll();
    history.unshift(data);
    if (history.length > 100) history.pop();
    localStorage.setItem('pp_history', JSON.stringify(history));
    histIdx = -1;
    $('sendInput').value = '';
    $('sendInput').focus();
  } catch (e) {
    vText.push({ ts: nowTs(), s: `!! 发送失败: ${e}`, kind: 'sys' });
    renderActive(true); autoScroll();
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

// ---------- 快捷发送按钮 ----------
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
        vText.push({ ts: nowTs(), s: `→ [${q.label}] ${q.hex ? '(HEX) ' + q.data : q.data}`, kind: 'tx' });
        renderActive(true); autoScroll();
      } catch (e) { vText.push({ ts: nowTs(), s: `!! ${e}`, kind: 'sys' }); renderActive(true); }
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
function autoScroll() {
  if ($('scrollChk').checked) { const v = active(); v.render(true); v.scrollBottom(); }
}
refreshPorts();
vText.push({ ts: nowTs(), s: '── PortPilot 0.1.0 就绪(虚拟滚动版)──', kind: 'sys' });
renderActive(true);
