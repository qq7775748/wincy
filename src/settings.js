const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const hkEl = document.getElementById('hk');
const sizeEl = document.getElementById('size');
const pasteEl = document.getElementById('paste');
const keepEl = document.getElementById('keep');
const bootEl = document.getElementById('boot');
const posEl = document.getElementById('pos');
const themeEl = document.getElementById('theme');
const accentEl = document.getElementById('accent');
const statusEl = document.getElementById('status1');

let settings = null;
let recording = false;

function flash(msg, isErr) {
  statusEl.textContent = msg;
  statusEl.className = 'status ' + (isErr ? 'err' : 'ok');
  if (!isErr) setTimeout(() => { statusEl.textContent = ''; }, 1800);
}

function paintToggles() {
  pasteEl.classList.toggle('on', !!settings.paste_automatically);
  keepEl.classList.toggle('on', !!settings.keep_open);
}

async function persist() {
  try {
    settings = await invoke('save_settings', { settings });
    flash('已保存');
  } catch (e) {
    flash(String(e), true);
  }
}

// ------------------------------------------------------------------ hotkey UI

const MOD_ONLY = ['Control', 'Alt', 'Shift', 'Meta', 'AltGraph'];

function comboFrom(e) {
  const parts = [];
  if (e.ctrlKey) parts.push('Ctrl');
  if (e.altKey) parts.push('Alt');
  if (e.shiftKey) parts.push('Shift');
  if (e.metaKey) parts.push('Super');

  let key = null;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  else if (/^F[0-9]{1,2}$/.test(e.code)) key = e.code;
  else if (e.code === 'Space') key = 'Space';
  else if (e.code === 'Backquote') key = '`';
  else if (e.code === 'Minus') key = '-';
  else if (e.code === 'Equal') key = '=';
  else if (e.code === 'Semicolon') key = ';';
  else if (e.code === 'Quote') key = "'";
  else if (e.code === 'Comma') key = ',';
  else if (e.code === 'Period') key = '.';
  else if (e.code === 'Slash') key = '/';
  else if (e.code === 'Backslash') key = '\\';
  else if (e.code === 'BracketLeft') key = '[';
  else if (e.code === 'BracketRight') key = ']';
  else if (e.code === 'ArrowUp') key = 'Up';
  else if (e.code === 'ArrowDown') key = 'Down';
  else if (e.code === 'ArrowLeft') key = 'Left';
  else if (e.code === 'ArrowRight') key = 'Right';

  if (!key) return null;
  if (!parts.length) return null;
  parts.push(key);
  return parts.join('+');
}

hkEl.addEventListener('click', () => {
  recording = true;
  hkEl.classList.add('recording');
  hkEl.textContent = '按下组合键…';
});

window.addEventListener('keydown', async (e) => {
  if (!recording) return;
  e.preventDefault();
  e.stopPropagation();
  if (e.key === 'Escape') {
    recording = false;
    hkEl.classList.remove('recording');
    hkEl.textContent = settings.hotkey;
    return;
  }
  if (MOD_ONLY.includes(e.key)) {
    const mods = [];
    if (e.ctrlKey) mods.push('Ctrl');
    if (e.altKey) mods.push('Alt');
    if (e.shiftKey) mods.push('Shift');
    if (e.metaKey) mods.push('Super');
    hkEl.textContent = (mods.join('+') + '+…') || '按下组合键…';
    return;
  }
  const combo = comboFrom(e);
  if (!combo) {
    flash('需要至少一个修饰键（Ctrl / Alt / Shift）', true);
    return;
  }
  recording = false;
  hkEl.classList.remove('recording');
  const prev = settings.hotkey;
  settings.hotkey = combo;
  hkEl.textContent = combo;
  try {
    settings = await invoke('save_settings', { settings });
    flash('快捷键已更新');
  } catch (err) {
    settings.hotkey = prev;
    hkEl.textContent = prev;
    flash(String(err), true);
  }
});

// --------------------------------------------------------------- other fields

sizeEl.addEventListener('change', async () => {
  let v = parseInt(String(sizeEl.value).replace(/[^\d]/g, ''), 10);
  // No upper bound: the only rule is a sane minimum, and 0 means "keep all".
  if (!Number.isFinite(v) || v < 0) v = 200;
  if (v > 0 && v < 10) v = 10;
  sizeEl.value = v;
  settings.history_size = v;
  await persist();
});

const ACCENT_LABEL = {
  blue: '蓝', indigo: '靛', purple: '紫', pink: '粉',
  red: '红', orange: '橙', green: '绿', graphite: '石墨'
};

function paintAccents(active) {
  accentEl.textContent = '';
  (window.__teecyTheme.ACCENTS || []).forEach((a) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'sw' + (a === active ? ' on' : '');
    b.dataset.accent = a;
    b.title = ACCENT_LABEL[a] || a;
    b.addEventListener('click', async () => {
      // Local first so the swatch ring and the whole window recolour instantly,
      // then persist — the popup follows through the shared localStorage.
      window.__teecyTheme.setAccent(a);
      paintAccents(a);
      settings.accent = a;
      await persist();
    });
    accentEl.appendChild(b);
  });
}

themeEl.addEventListener('change', async () => {
  const v = themeEl.value === 'dark' || themeEl.value === 'light' ? themeEl.value : 'system';
  settings.theme = v;
  window.__teecyTheme.set(v);
  await persist();
});

pasteEl.addEventListener('click', async () => {
  settings.paste_automatically = !settings.paste_automatically;
  pasteEl.classList.toggle('on', settings.paste_automatically);
  await persist();
});

keepEl.addEventListener('click', async () => {
  settings.keep_open = !settings.keep_open;
  keepEl.classList.toggle('on', settings.keep_open);
  await persist();
});

posEl.addEventListener('change', async () => {
  settings.position = posEl.value;
  await persist();
});

// The popup switches itself to "custom" when the user drags it.
listen('settings-changed', async () => {
  const s = await invoke('get_settings');
  settings.position = s.position;
  posEl.value = s.position;
});

bootEl.addEventListener('click', async () => {
  const next = !bootEl.classList.contains('on');
  try {
    const on = await invoke('set_autostart', { enabled: next });
    bootEl.classList.toggle('on', on);
    settings.launch_at_login = on;
    await persist();
  } catch (e) {
    flash(String(e), true);
  }
});

document.getElementById('clear').addEventListener('click', async () => {
  await invoke('clear_history', { includePinned: false });
  flash('已清空（保留固定项）');
});

document.getElementById('clearAll').addEventListener('click', async () => {
  if (!confirm('清空全部历史（含固定项）？此操作不可恢复。')) return;
  await invoke('clear_history', { includePinned: true });
  flash('已清空全部');
});

document.getElementById('openDir').addEventListener('click', () => invoke('open_data_dir'));
document.getElementById('close').addEventListener('click', () => invoke('close_settings'));
document.getElementById('quit').addEventListener('click', () => invoke('quit_app'));

window.addEventListener('keydown', (e) => {
  if (e.key === 'Escape' && !recording) invoke('close_settings');
});

async function load() {
  settings = await invoke('get_settings');
  hkEl.textContent = settings.hotkey;
  sizeEl.value = settings.history_size;
  posEl.value = settings.position || 'cursor';
  const t = settings.theme === 'dark' || settings.theme === 'light' ? settings.theme : 'system';
  themeEl.value = t;
  // The backend is the source of truth; mirror it so the popup can pick the
  // right palette without waiting for IPC on every hotkey press.
  window.__teecyTheme.applyAll(t, settings.accent);
  paintAccents(window.__teecyTheme.accent());
  paintToggles();
  const auto = await invoke('get_autostart');
  bootEl.classList.toggle('on', auto);
}

load();
