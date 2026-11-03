// PortPilot 前端 0.2.0 · 多标签 / 虚拟滚动 / 规则表 / 示波器 / CRC

// 全局错误捕获:release 版无 DevTools,JS 错误必须可见(先缓存,终端就绪后冲刷)
window._jsErrors = [];
window.addEventListener('error', (e) => {
  const m = 'JSERR: ' + e.message + ' @' + e.filename?.split(/[\\/]/).pop() + ':' + e.lineno;
  window._jsErrors.push(m);
  try { window.__PP_sysLine && window.__PP_sysLine(m); } catch {}
});
window.addEventListener('unhandledrejection', (e) => {
  const m = 'REJECT: ' + String(e.reason);
  window._jsErrors.push(m);
  try { window.__PP_sysLine && window.__PP_sysLine(m); } catch {}
});

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const LH = 18;
const MAX_STORED = 1000000;
const TRUNC = 4096;

// ============================================================
// 虚拟滚动日志(每个标签一对:文本 + HEX)
// ============================================================
class VirtualLog {
  constructor(viewportId) {
    this.viewport = $(viewportId);
    this.inner = this.viewport.firstElementChild;
    this.lines = []; this.visible = [];
    this.max = MAX_STORED; this.dropped = 0;
    this.filter = null; this.dirty = true;
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
  applyFilter(f) { this.filter = f; this.visible = this.lines.filter((l) => !this.hide(l)); this.dirty = true; }
  clear() { this.lines = []; this.visible = []; this.dropped = 0; this.dirty = true; }
  count() { return { visible: this.visible.length, stored: this.lines.length, dropped: this.dropped }; }
  wasAtBottom() { return this.viewport.scrollTop + this.viewport.clientHeight >= this.inner.offsetHeight - LH * 2; }
  scrollBottom() { this.viewport.scrollTop = this.inner.offsetHeight; }
  render(force) {
    if (!this.dirty && !force) return;
    this.dirty = false;
    const vh = this.viewport.clientHeight;
    const total = this.visible.length;
    this.inner.style.height = total * LH + 'px';
    let first = Math.floor(this.viewport.scrollTop / LH) - 5;
    let count = Math.ceil(vh / LH) + 10;
    first = Math.max(0, Math.min(first, Math.max(0, total - count)));
    const end = Math.min(total, first + count);
    for (let i = 0; i < this.pool.length; i++) this.pool[i].style.display = 'none';
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
  if (filter && filter.mode === 'include' && filter.markRe) return e.replace(filter.markRe, (m) => `<mark>${m}</mark>`);
  return e;
}
function buildFilter(text, mode) {
  const t = text.trim();
  if (!t) return null;
  if (t.startsWith('re:')) {
    let re; try { re = new RegExp(t.slice(3), 'i'); } catch { return null; }
    let mark; try { mark = new RegExp(t.slice(3), 'gi'); } catch { mark = null; }
    return { mode, testRe: re, markRe: mark };
  }
  const escd = t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  return { mode, testRe: new RegExp(escd, 'i'), markRe: new RegExp(escd, 'gi') };
}

// ============================================================
// 多标签
// ============================================================
const tabs = new Map();   // id -> tab 对象
let activeId = null;
let tabSeq = 0;

function createTab(name) {
  const id = 't' + (++tabSeq) + '_' + Math.random().toString(36).slice(2, 6);
  const host = $('termHost');
  const wrapT = document.createElement('div');
  wrapT.className = 'term hidden'; wrapT.id = 'termText-' + id;
  wrapT.innerHTML = '<div class="inner"></div>';
  const wrapH = document.createElement('div');
  wrapH.className = 'term hidden'; wrapH.id = 'termHex-' + id;
  wrapH.innerHTML = '<div class="inner"></div>';
  host.appendChild(wrapT); host.appendChild(wrapH);
  const tab = {
    id, name: name || `标签${tabSeq}`, connected: false, port: '', baud: 0, paused: false,
    vText: new VirtualLog('termText-' + id),
    vHex: new VirtualLog('termHex-' + id),
    wrapT, wrapH,
  };
  tabs.set(id, tab);
  renderTabChips();
  activateTab(id);
  return tab;
}
function closeTab(id) {
  const tab = tabs.get(id);
  if (!tab) return;
  if (tab.connected) invoke('disconnect', { tab: id }).catch(() => {});
  tab.wrapT.remove(); tab.wrapH.remove();
  tabs.delete(id);
  tabOrder();
  if (activeId === id) {
    const next = [...tabs.keys()][0];
    if (next) activateTab(next);
    else createTab();
  }
  renderTabChips();
}
function tabOrder() { /* tabs Map 保持插入序,无需额外处理 */ }
function activateTab(id) {
  activeId = id;
  for (const [tid, tab] of tabs) {
    tab.wrapT.classList.toggle('hidden', tid !== id || $('hexChk').checked);
    tab.wrapH.classList.toggle('hidden', tid !== id || !$('hexChk').checked);
  }
  renderTabChips();
  const tab = tabs.get(id);
  if (tab) { tab.vText.render(true); tab.vHex.render(true); }
  // 过滤器跟随标签
  $('filterInput').value = tab?.filterText || '';
  $('filterMode').value = tab?.filterMode || 'include';
  applyFilterToActive();
  updateLineStat();
}
function renderTabChips() {
  const chips = $('tabChips');
  chips.innerHTML = '';
  for (const [id, tab] of tabs) {
    const chip = document.createElement('span');
    chip.className = 'chip' + (id === activeId ? ' active' : '');
    chip.innerHTML =
      `<span class="led ${tab.connected ? 'on' : ''}"></span>` +
      `<span>${esc(tab.name)}</span><span class="x" title="关闭">✕</span>`;
    chip.onclick = (e) => {
      if (e.target.classList.contains('x')) closeTab(id);
      else activateTab(id);
    };
    chips.appendChild(chip);
  }
}
function activeTab() { return tabs.get(activeId); }
function activeLogs() {
  const t = activeTab();
  return $('hexChk').checked ? t.vHex : t.vText;
}

$('tabAdd').onclick = () => createTab();

// ---------- 过滤(作用于当前标签) ----------
let filterTimer = null;
function applyFilterToActive() {
  const t = activeTab();
  if (!t) return;
  t.filterText = $('filterInput').value;
  t.filterMode = $('filterMode').value;
  t.vText.applyFilter(buildFilter(t.filterText, t.filterMode));
}
$('filterInput').addEventListener('input', () => {
  clearTimeout(filterTimer);
  filterTimer = setTimeout(() => { applyFilterToActive(); renderActive(true); }, 200);
});
$('filterMode').onchange = () => { applyFilterToActive(); renderActive(true); };

// ---------- 编码 ----------
$('encSel').onchange = async () => {
  const t = activeTab();
  if (t && t.connected) {
    try { await invoke('set_encoding', { tab: t.id, encoding: $('encSel').value }); } catch (e) { sysLine(t, `!! ${e}`); }
  }
};

// ---------- 工具栏 ----------
$('hexChk').onchange = () => {
  const t = activeTab();
  t.wrapT.classList.toggle('hidden', $('hexChk').checked);
  t.wrapH.classList.toggle('hidden', !$('hexChk').checked);
  renderActive(true);
  if ($('scrollChk').checked) activeLogs().scrollBottom();
};
$('pauseBtn').onclick = () => {
  const t = activeTab();
  t.paused = !t.paused;
  $('pauseBtn').textContent = t.paused ? '▶' : '⏸';
  $('pauseBtn').classList.toggle('on', t.paused);
  renderActive(true);
};
$('clearBtn').onclick = () => {
  const t = activeTab();
  t.vText.clear(); t.vHex.clear();
  renderActive(true);
};

// ============================================================
// 接收事件(按标签路由)
// ============================================================
listen('rx', (e) => {
  const d = e.payload;
  const tab = tabs.get(d.tab);
  if (!tab) return;
  if (d.replay_progress) $('replayStatus').textContent = `回放中 ${d.replay_progress}`;
  if (d.replay_done) $('replayStatus').textContent = '回放完成 ✓';
  for (const l of d.lines || []) {
    if (l.s && l.s.includes('脚本执行完成')) {
      scriptRunning = false;
      $('scriptRun').textContent = '运行';
    }
  }
  const stick = tab.vText.wasAtBottom();
  for (const l of d.lines) tab.vText.push({ ts: l.ts, s: l.s, kind: 'rx' });
  if (d.hex) {
    const ts = new Date().toISOString().slice(11, 23);
    tab.vHex.push({ ts, s: d.hex, kind: 'hex' });
  }
  if (tab.id === activeId) {
    if (!tab.paused) {
      renderActive();
      if ($('scrollChk').checked && stick) { activeLogs().render(true); activeLogs().scrollBottom(); }
    }
    updateLineStat();
  }
});
listen('scope', (e) => {
  const d = e.payload;
  let arr = scopeData.get(d.tab);
  if (!arr) { arr = []; scopeData.set(d.tab, arr); }
  for (const ch of d.frames) arr.push({ t: performance.now(), ch });  // 全量帧入库(不抽稀)
  while (arr.length > 1200) arr.shift();
  if (!$('scopePanel').classList.contains('hidden') && d.tab === activeId) scheduleScopeDraw();
});
function renderActive(force) {
  const t = activeTab();
  if (!t) return;
  t.vHex.render(force);
  t.vText.render(force);
  updateLineStat();
}
function updateLineStat() {
  const t = activeTab();
  if (!t) return;
  const c = t.vText.count();
  $('lineStat').textContent = `${c.visible} 行 · 存 ${c.stored}${c.dropped ? ` · 丢旧 ${c.dropped}` : ''}`;
}

// ============================================================
// 连接
// ============================================================
// 传输类型切换:串口/网络地址输入切换
$('kindSel').onchange = () => {
  const k = $('kindSel').value;
  $('serialCtrls').style.display = k === 'serial' ? 'contents' : 'none';
  $('netCtrls').style.display = (k === 'serial' || k === 'loop') ? 'none' : 'contents';
  $('netRemote').style.display = k === 'udp' ? '' : 'none';
  $('netAddr').placeholder = k === 'tcp_client' ? '目标 host:port,如 192.168.1.10:8080'
    : k === 'tcp_server' ? '本地绑定,如 0.0.0.0:8080' : '本地绑定,如 0.0.0.0:9000';
};

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

async function setConnectedUI(tab, on, info) {
  tab.connected = on;
  if (tab.id === activeId) {
    $('dot').className = 'dot ' + (on ? 'on' : 'off');
    $('connectBtn').textContent = on ? '断开' : '连接';
    $('connectBtn').classList.toggle('on', on);
    $('connInfo').textContent = info || '';
    $('connInfo').title = info || '';
  }
  renderTabChips();
}

$('connectBtn').onclick = async () => {
  const tab = activeTab();
  try {
    if (!tab.connected) {
      const kind = $('kindSel').value;
      const isSerial = kind === 'serial';
      const port = isSerial ? $('portSel').value : $('netAddr').value.trim();
      const baud = isSerial ? (Number($('customBaud').value) || Number($('baudSel').value)) : 0;
      if (!port) return alert(isSerial ? '未检测到串口,请先刷新' : '请填写地址');
      const remote = kind === 'udp' ? ($('netRemote').value.trim() || null) : null;
      const msg = await invoke('connect', { tab: tab.id, port, baud, encoding: $('encSel').value, kind, remote });
      tab.port = port; tab.baud = baud;
      tab.name = isSerial ? `${port}@${baud}` : `${kind === 'udp' ? 'UDP' : 'TCP'} ${port}`;
      await setConnectedUI(tab, true, msg);
      tab.vText.push({ ts: nowTs(), s: `── 已连接 ${port} @ ${baud} ──`, kind: 'sys' });
      $('scopeOn').checked && (await invoke('set_scope', { on: true, channels: Number($('scopeCh').value), protocol: $('scopeProto').value }));
    } else {
      const msg = await invoke('disconnect', { tab: tab.id });
      await setConnectedUI(tab, false, '');
      tab.vText.push({ ts: nowTs(), s: `── ${msg} ──`, kind: 'sys' });
    }
    renderActive(true); autoScroll();
  } catch (e) {
    activeTab().vText.push({ ts: nowTs(), s: `!! ${e}`, kind: 'sys' });
    renderActive(true); autoScroll();
  }
};

$('dtrBtn').onclick = async () => {
  const t = activeTab();
  const on = !$('dtrBtn').classList.contains('on');
  $('dtrBtn').classList.toggle('on', on);
  try { await invoke('set_dtr', { tab: t.id, on }); } catch (e) { sysLine(t, `!! ${e}`); }
};
$('rtsBtn').onclick = async () => {
  const t = activeTab();
  const on = !$('rtsBtn').classList.contains('on');
  $('rtsBtn').classList.toggle('on', on);
  try { await invoke('set_rts', { tab: t.id, on }); } catch (e) { sysLine(t, `!! ${e}`); }
};
function sysLine(tab, s) {
  tab.vText.push({ ts: nowTs(), s, kind: 'sys' });
  if (tab.id === activeId) { renderActive(true); autoScroll(); }
  window.__PP_sysLine = (m) => sysLine(tab, m); // 全局错误上报复用最近活动标签
}

// 每秒状态栏(仅当前标签)
setInterval(async () => {
  const t = activeTab();
  if (!t) return;
  try {
    const st = await invoke('stats', { tab: t.id });
    const rate = st.rx_bytes - (t._lastRx || 0);
    t._lastRx = st.rx_bytes;
    $('statTxt').textContent =
      `RX ${st.rx_bytes} (${rate}/s) · TX ${st.tx_bytes} · 重连 ${st.reconnects}` +
      (st.backpressure ? ` · 背压 ${st.backpressure}` : '');
    if (st.connected !== t.connected) {
      await setConnectedUI(t, st.connected, st.connected ? '已连接' : '');
    }
  } catch {}
}, 1000);

// ============================================================
// 连接档案
// ============================================================
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
    kind: $('kindSel').value, addr: $('netAddr').value.trim(), remote: $('netRemote').value.trim(),
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

// ============================================================
// 发送 + CRC
// ============================================================
const history = JSON.parse(localStorage.getItem('pp_history') || '[]');
let histIdx = -1;
function nowTs() { return new Date().toISOString().slice(11, 23); }

let crcTimer = null;
async function updateCrcPreview() {
  const kind = $('checksumSel').value;
  const pv = $('checksumPreview');
  if (kind === 'none' || !$('sendInput').value) { pv.textContent = ''; return; }
  clearTimeout(crcTimer);
  crcTimer = setTimeout(async () => {
    try {
      const hex = await invoke('compute_checksum', {
        data: $('sendInput').value,
        hexMode: $('sendHexChk').checked,
        checksum: kind,
        endian: $('endianSel').value,
      });
      pv.textContent = '校验: ' + hex;
    } catch { pv.textContent = ''; }
  }, 150);
}
$('sendInput').addEventListener('input', updateCrcPreview);
$('checksumSel').onchange = updateCrcPreview;
$('endianSel').onchange = updateCrcPreview;
$('sendHexChk').onchange = updateCrcPreview;

async function doSend() {
  const tab = activeTab();
  const data = $('sendInput').value;
  if (!data) return;
  try {
    await invoke('send', {
      tab: tab.id, data,
      hexMode: $('sendHexChk').checked,
      newline: $('nlSel').value,
      checksum: $('checksumSel').value,
      endian: $('endianSel').value,
    });
    const disp = $('sendHexChk').checked
      ? '(HEX) ' + data.replace(/[^0-9a-fA-F]/g, '').toUpperCase()
      : data + { cr: '\\r', lf: '\\n', crlf: '\\r\\n', none: '' }[$('nlSel').value];
    tab.vText.push({ ts: nowTs(), s: '→ ' + disp, kind: 'tx' });
    renderActive(true); autoScroll();
    history.unshift(data);
    if (history.length > 100) history.pop();
    localStorage.setItem('pp_history', JSON.stringify(history));
    histIdx = -1;
    $('sendInput').value = '';
    updateCrcPreview();
    $('sendInput').focus();
  } catch (e) {
    sysLine(tab, `!! 发送失败: ${e}`);
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

// ---------- 快捷按钮 ----------
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
      const tab = activeTab();
      try {
        await invoke('send', { tab: tab.id, data: q.data, hexMode: q.hex, newline: q.nl, checksum: 'none', endian: 'little' });
        tab.vText.push({ ts: nowTs(), s: `→ [${q.label}] ${q.hex ? '(HEX) ' + q.data : q.data}`, kind: 'tx' });
        renderActive(true); autoScroll();
      } catch (e) { sysLine(tab, `!! ${e}`); }
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

// ============================================================
// 协议解码面板
// ============================================================
function hexToBytes(s) {
  const c = (s || '').replace(/[^0-9a-fA-F]/g, '');
  const out = [];
  for (let i = 0; i + 1 < c.length; i += 2) out.push(parseInt(c.substr(i, 2), 16));
  return out;
}
const DEC_PRESETS = {
  nmea: {
    header: '24', frameMode: 'tail', tail: '0a', checksum: 'none',
    fields: [{ name: 'sentence', offset: 0, len: 0, kind: 'str', le: false }],
  },
  modbus8: {
    header: '', frameMode: 'len', len: 8, checksum: 'crc16_modbus',
    fields: [
      { name: 'addr', offset: 0, len: 1, kind: 'uint', le: false },
      { name: 'func', offset: 1, len: 1, kind: 'uint', le: false },
      { name: 'value', offset: 2, len: 2, kind: 'uint', le: true },
    ],
  },
};
function decRows() {
  return [...$('decFields').children].map((row) => ({
    name: row.querySelector('.fname').value,
    off: Number(row.querySelector('.foff').value) || 0,
    len: Number(row.querySelector('.flen').value) || 0,
    kind: row.querySelector('.fkind').value,
    le: row.querySelector('.fle').checked,
  }));
}
function buildDecoderConfig() {
  if (!$('decoderEnable').checked) return null;
  const mode = $('decFrameMode').value;
  const cfg = {
    name: 'custom',
    header: hexToBytes($('decHeader').value),
    frame_len: null,
    tail: [],
    fields: decRows(),
    checksum: $('decChecksum').value,
  };
  if (mode === 'len') cfg.frame_len = Number($('decLen').value) || null;
  else cfg.tail = hexToBytes($('decTail').value);
  return cfg;
}
$('decTpl').onchange = () => {
  const pre = DEC_PRESETS[$('decTpl').value];
  if (!pre) return;
  $('decHeader').value = pre.header;
  $('decFrameMode').value = pre.frameMode;
  $('decLen').value = pre.len || '';
  $('decTail').value = pre.tail ? pre.tail.map((b) => b.toString(16).padStart(2, '0')).join(' ') : '';
  $('decChecksum').value = pre.checksum;
  const rows = $('decFields');
  rows.innerHTML = '';
  for (const f of pre.fields) rows.appendChild(decRowEl(f));
};
$('decFrameMode').onchange = () => {
  $('decLen').style.display = $('decFrameMode').value === 'len' ? '' : 'none';
  $('decTail').style.display = $('decFrameMode').value === 'tail' ? '' : 'none';
};
$('decoderEnable').onchange = async () => {
  const tab = activeTab();
  try {
    await invoke('set_decoder', { tab: tab.id, config: buildDecoderConfig() });
  } catch (e) { alert(e); }
};
$('decLen').oninput = $('decHeader').oninput = $('decTail').oninput = $('decChecksum').onchange = async () => {
  if ($('decoderEnable').checked) {
    try { await invoke('set_decoder', { tab: activeId, config: buildDecoderConfig() }); } catch {}
  }
};
function decRowEl(f) {
  const div = document.createElement('div');
  div.className = 'frow';
  div.innerHTML = `
    <input type="text" class="fname" placeholder="字段名" value="${esc(f.name)}" />
    <input type="number" class="foff" placeholder="偏移" value="${f.offset}" title="偏移(帧头后)" />
    <input type="number" class="flen" placeholder="长度" value="${f.len}" title="长度(Str 填 0=剩余)" />
    <select class="fkind">
      <option value="uint" ${f.kind === 'uint' ? 'selected' : ''}>uint</option>
      <option value="int" ${f.kind === 'int' ? 'selected' : ''}>int</option>
      <option value="float" ${f.kind === 'float' ? 'selected' : ''}>float</option>
      <option value="bytes" ${f.kind === 'bytes' ? 'selected' : ''}>bytes</option>
      <option value="str" ${f.kind === 'str' ? 'selected' : ''}>str</option>
    </select>
    <label class="dim">LE<input type="checkbox" class="fle" ${f.le ? 'checked' : ''} /></label>
    <button class="ghost fdel" title="删除">✕</button>`;
  div.querySelector('.fdel').onclick = () => div.remove();
  return div;
}
$('decAddField').onclick = () =>
  $('decFields').appendChild(decRowEl({ name: '', offset: 0, len: 1, kind: 'uint', le: false }));
$('decClear').onclick = () => { decFrames = []; renderDecTable(); };

let decFrames = [];
listen('decoded', (e) => {
  const d = e.payload;
  if (d.tab !== activeId) return;
  const ts = nowTs();
  for (const f of d.frames) {
    decFrames.push({ ts, ok: f.ok, fields: f.fields, raw: f.raw });
    if (decFrames.length > 50) decFrames.shift();
  }
  renderDecTable();
});
function renderDecTable() {
  const host = $('decOut');
  if (!decFrames.length) {
    host.innerHTML = '<span class="dim">暂无解码帧</span>';
    $('decInfo').textContent = '';
    return;
  }
  const uni = [...new Set(decFrames.flatMap((f) => f.fields.map((x) => x[0])))];
  let html = '<table><tr><th>时间</th><th>✓</th>';
  for (const n of uni) html += `<th>${esc(n)}</th>`;
  html += '<th>HEX</th></tr>';
  for (const f of [...decFrames].reverse()) {
    html += `<tr class="ok-${f.ok}"><td>${esc(f.ts)}</td><td>${f.ok ? '✓' : '✗'}</td>`;
    const map = new Map(f.fields);
    for (const n of uni) html += `<td>${esc(map.get(n) ?? '')}</td>`;
    html += `<td class="hexcell">${esc(f.raw.map((b) => b.toString(16).padStart(2, '0')).join(' '))}</td></tr>`;
  }
  host.innerHTML = html + '</table>';
  $('decInfo').textContent = `${decFrames.length} 帧`;
}

// ============================================================
// Lua 脚本
// ============================================================
const SCRIPT_EXAMPLE = `-- 示例:每 2s 发 AT 并等待 OK(超时 3s)
for i = 1, 5 do
  if stop_requested() then break end
  port.write("AT" .. i)
  local resp = port.expect("OK", 3000)
  if resp and resp:find("OK") then
    log("第 " .. i .. " 次:收到 OK")
  else
    log("第 " .. i .. " 次:超时未收到 OK")
  end
  sleep(2000)
end
log("示例结束")`;
let scriptRunning = false;

$('scriptBtn').onclick = () => {
  $('scriptModal').classList.remove('hidden');
  if (!$('scriptCode').value) {
    $('scriptCode').value = localStorage.getItem('pp_script') || SCRIPT_EXAMPLE;
  }
};
$('scriptClose').onclick = () => $('scriptModal').classList.add('hidden');
$('scriptModal').addEventListener('click', (e) => {
  if (e.target === $('scriptModal') && !scriptRunning) $('scriptModal').classList.add('hidden');
});
$('scriptExample').onclick = () => { $('scriptCode').value = SCRIPT_EXAMPLE; };
$('scriptRun').onclick = async () => {
  if (scriptRunning) return;
  const tab = activeTab();
  if (!tab.connected) return alert('请先连接');
  const code = $('scriptCode').value;
  localStorage.setItem('pp_script', code);
  scriptRunning = true;
  $('scriptRun').textContent = '运行中…';
  try {
    await invoke('run_script', { tab: tab.id, code });
  } catch (e) {
    sysLine(tab, `!! 脚本启动失败: ${e}`);
    scriptRunning = false;
    $('scriptRun').textContent = '运行';
  }
};
$('scriptStop').onclick = async () => {
  const tab = activeTab();
  try { await invoke('stop_script', { tab: tab.id }); } catch {}
  scriptRunning = false;
  $('scriptRun').textContent = '运行';
};

// ============================================================
// 日志回放
// ============================================================
$('replayBtn').onclick = async () => {
  try {
    const logs = await invoke('list_logs');
    const sel = $('replayFile');
    sel.innerHTML = '';
    for (const l of logs) {
      const o = document.createElement('option');
      o.value = l.file;
      o.textContent = `${l.file}  (${(l.size / 1024).toFixed(1)} KB)`;
      sel.appendChild(o);
    }
    $('replayStatus').textContent = logs.length ? '' : '(logs 目录为空,先连接一次产生日志)';
    $('replayModal').classList.remove('hidden');
  } catch (e) { alert(e); }
};
$('replayClose').onclick = () => $('replayModal').classList.add('hidden');
$('replayModal').addEventListener('click', (e) => {
  if (e.target === $('replayModal')) $('replayModal').classList.add('hidden');
});
$('replayStart').onclick = async () => {
  const file = $('replayFile').value;
  if (!file) return alert('请选择日志文件');
  try {
    const msg = await invoke('replay', { tab: activeId, file, speed: Number($('replaySpeed').value) });
    $('replayStatus').textContent = msg;
  } catch (e) { $('replayStatus').textContent = String(e); }
};
$('replayStop').onclick = async () => {
  try { await invoke('replay_stop', { tab: activeId }); } catch {}
};

// ============================================================
// 自动应答规则表
// ============================================================
function loadRules() { return JSON.parse(localStorage.getItem('pp_rules') || '[]'); }
function saveRulesStorage(rules) { localStorage.setItem('pp_rules', JSON.stringify(rules)); }

$('rulesBtn').onclick = () => { renderRulesModal(); $('rulesModal').classList.remove('hidden'); };
$('rulesClose').onclick = () => $('rulesModal').classList.add('hidden');
$('rulesModal').addEventListener('click', (e) => {
  if (e.target === $('rulesModal')) $('rulesModal').classList.add('hidden');
});
$('rulesAdd').onclick = () => {
  $('rulesRows').appendChild(ruleRow({ enabled: true, kind: 'text', pattern: '', payload: '', hex: false, newline: 'none' }));
};
$('rulesSave').onclick = async () => {
  const rows = [...$('rulesRows').children].map((row) => ({
    enabled: row.querySelector('.en').checked,
    kind: row.querySelector('.kind').value,
    pattern: row.querySelector('.pat').value,
    payload: row.querySelector('.pay').value,
    hex: row.querySelector('.hx').checked,
    newline: row.querySelector('.nls').value,
  }));
  saveRulesStorage(rows);
  await invoke('set_rules', { rules: rows });
  $('rulesModal').classList.add('hidden');
};
function ruleRow(r) {
  const div = document.createElement('div');
  div.className = 'rule-row';
  div.innerHTML = `
    <input type="checkbox" class="en" ${r.enabled ? 'checked' : ''} title="启用" />
    <select class="kind">
      <option value="text" ${r.kind === 'text' ? 'selected' : ''}>文本</option>
      <option value="hex" ${r.kind === 'hex' ? 'selected' : ''}>HEX</option>
      <option value="regex" ${r.kind === 'regex' ? 'selected' : ''}>正则</option>
    </select>
    <input type="text" class="pat" placeholder="匹配关键词" value="${escAttr(r.pattern)}" />
    <input type="text" class="pay" placeholder="发送内容" value="${escAttr(r.payload)}" />
    <label><input type="checkbox" class="hx" ${r.hex ? 'checked' : ''} />HEX</label>
    <select class="nls">
      <option value="none" ${r.newline === 'none' ? 'selected' : ''}>无</option>
      <option value="cr" ${r.newline === 'cr' ? 'selected' : ''}>\\r</option>
      <option value="lf" ${r.newline === 'lf' ? 'selected' : ''}>\\n</option>
      <option value="crlf" ${r.newline === 'crlf' ? 'selected' : ''}>\\r\\n</option>
    </select>
    <button class="ghost del" title="删除">✕</button>`;
  div.querySelector('.del').onclick = () => div.remove();
  return div;
}
function escAttr(s) { return String(s).replace(/"/g, '&quot;').replace(/&/g, '&amp;').replace(/</g, '&lt;'); }
(async function initRules() {
  const rules = loadRules();
  if (rules.length) {
    try { await invoke('set_rules', { rules }); } catch {}
    for (const r of rules) $('rulesRows').appendChild(ruleRow(r));
  }
})();

// ============================================================
// 示波器
// ============================================================
const scopeData = new Map();   // tab -> frames[{t, ch[]}]
const CH_COLORS = ['#4cc2ff', '#ffd166', '#7ce38b', '#ff8fa3', '#c9a7ff', '#ffb37c', '#8ef0d1', '#ff9f8f'];
let scopeDrawScheduled = false;

$('scopeBtn').onclick = () => {
  try {
    const p = $('scopePanel');
    p.classList.toggle('hidden');
    p.style.display = p.classList.contains('hidden') ? 'none' : 'flex';
    const open = !p.classList.contains('hidden');
    if (open) {
      requestAnimationFrame(() => { resizeScopeCanvas(); drawScope(); });
      sysLine(activeTab(), '── 示波器面板: 已打开 ──');
    } else {
      sysLine(activeTab(), '── 示波器面板: 已关闭 ──');
    }
  } catch (e) {
    sysLine(activeTab(), `!! 示波器打开失败: ${e}`);
  }
};
$('scopeOn').onchange = async () => {
  try {
    await invoke('set_scope', {
      on: $('scopeOn').checked,
      channels: Number($('scopeCh').value),
      protocol: $('scopeProto').value,
    });
  } catch (e) { alert(e); }
};
$('scopeProto').onchange = $('scopeCh').onchange = async () => {
  if ($('scopeOn').checked) {
    try {
      await invoke('set_scope', {
        on: true, channels: Number($('scopeCh').value), protocol: $('scopeProto').value,
      });
    } catch (e) { alert(e); }
  }
};
$('scopeClear').onclick = () => { scopeData.set(activeId, []); drawScope(); };

function resizeScopeCanvas() {
  const c = $('scopeCanvas');
  const dpr = window.devicePixelRatio || 1;
  c.width = c.clientWidth * dpr;
  c.height = c.clientHeight * dpr;
  c.getContext('2d').setTransform(dpr, 0, 0, dpr, 0, 0);
}
window.addEventListener('resize', () => {
  if (!$('scopePanel').classList.contains('hidden')) { resizeScopeCanvas(); drawScope(); }
});

function scheduleScopeDraw() {
  if (scopeDrawScheduled) return;
  scopeDrawScheduled = true;
  requestAnimationFrame(() => { scopeDrawScheduled = false; drawScope(); });
}
function drawScope() {
  if ($('scopePanel').classList.contains('hidden')) return;
  const c = $('scopeCanvas');
  const ctx = c.getContext('2d');
  const W = c.clientWidth, H = c.clientHeight;
  ctx.clearRect(0, 0, W, H);
  // 网格
  ctx.strokeStyle = '#22262f';
  ctx.beginPath();
  for (let x = 0; x <= W; x += W / 10) { ctx.moveTo(x, 0); ctx.lineTo(x, H); }
  for (let y = 0; y <= H; y += H / 6) { ctx.moveTo(0, y); ctx.lineTo(W, y); }
  ctx.stroke();
  const frames = scopeData.get(activeId) || [];
  if (!frames.length) {
    ctx.fillStyle = '#5a606a';
    ctx.font = '12px Segoe UI';
    ctx.fillText('等待数据…(勾选"启用抽头",下位机按 JustFloat/CSV 发送浮点)', 12, 20);
    $('scopeInfo').textContent = '';
    return;
  }
  // Y 自适应(全通道窗口极值)
  let mn = Infinity, mx = -Infinity;
  for (const f of frames) for (const v of f.ch) { if (v < mn) mn = v; if (v > mx) mx = v; }
  if (mn === mx) { mn -= 1; mx += 1; }
  const pad = (mx - mn) * 0.1;
  mn -= pad; mx += pad;
  // 通道数以最后一帧为准
  const chN = frames[frames.length - 1].ch.length;
  for (let cI = 0; cI < chN; cI++) {
    ctx.strokeStyle = CH_COLORS[cI % CH_COLORS.length];
    ctx.lineWidth = 1.4;
    ctx.beginPath();
    const n = frames.length;
    for (let i = 0; i < n; i++) {
      const v = frames[i].ch[cI];
      if (v === undefined) continue;
      const x = (i / (n - 1)) * (W - 8) + 4;
      const y = H - ((v - mn) / (mx - mn)) * (H - 16) - 8;
      i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
    }
    ctx.stroke();
  }
  // 图例
  ctx.font = '11px Consolas';
  for (let cI = 0; cI < chN; cI++) {
    const last = frames[frames.length - 1].ch[cI];
    ctx.fillStyle = CH_COLORS[cI % CH_COLORS.length];
    ctx.fillText(`CH${cI + 1} ${last !== undefined ? last.toFixed(2) : '-'}`, 10 + cI * 90, 12);
  }
  $('scopeInfo').textContent = `${frames.length} 帧 · Y[${mn.toFixed(1)}, ${mx.toFixed(1)}]`;
}
setInterval(() => {
  if (!$('scopePanel').classList.contains('hidden')) drawScope();
}, 100);

// ============================================================
// 启动
// ============================================================
function autoScroll() {
  if ($('scrollChk').checked) { const v = activeLogs(); v.render(true); v.scrollBottom(); }
}
refreshPorts();
createTab('标签1');
vText0Ready();
function vText0Ready() {
  const t = [...tabs.values()][0];
  if (t) {
    // 冲刷启动期捕获的 JS 错误(可视化,便于远程排障)
    for (const m of window._jsErrors.splice(0)) t.vText.push({ ts: nowTs(), s: m, kind: 'sys' });
    t.vText.push({ ts: nowTs(), s: '── PortPilot 0.2.0 就绪(P1:多标签/CRC/规则表/示波器)──', kind: 'sys' });
    window.__PP_sysLine = (m) => t.vText.push({ ts: nowTs(), s: m, kind: 'sys' });
    renderActive(true);
  }
}
