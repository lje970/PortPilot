// 渲染进程:接收批量数据 → 更新 DOM(即"上屏")→ 回报渲染事件。
const { ipcRenderer } = require('electron');

const counterEl = document.getElementById('counter');
const logEl = document.getElementById('log');
let renders = 0;
let rxTotal = 0;

ipcRenderer.on('batch', (_e, { id, bytes, preview, arrNewest }) => {
  renders++;
  rxTotal += bytes;
  counterEl.textContent = `${rxTotal} B`;
  const line = document.createElement('div');
  line.textContent = preview + (bytes > 64 ? ' …' : '');
  logEl.appendChild(line);
  while (logEl.childElementCount > 200) logEl.removeChild(logEl.firstChild);
  ipcRenderer.send('rendered', { id, arrNewest });
});

// 每秒回报渲染次数(= UI fps 口径,与探针协议一致)
setInterval(() => {
  ipcRenderer.send('fps', { renders });
  renders = 0;
}, 1000);
