const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const listEl = document.getElementById('list');
const qEl = document.getElementById('q');
const emptyEl = document.getElementById('empty');
const countEl = document.getElementById('count');
const scopeEl = document.getElementById('scope');

const SCOPES = ['all', 'text', 'image', 'files'];
const SCOPE_LABEL = { all: '全部', text: '文本', image: '图片', files: '文件' };

let all = [];
let view = [];
let sel = 0;
let scope = 'all';
// Cheap identity of `all`; lets a silent refresh skip the whole re-render.
let allSig = '';
// q|scope|allSig of the list that is currently in the DOM.
let viewKey = '';
let built = false;
// Rows are built in chunks: history can be set to "unlimited", and turning
// thousands of entries into DOM nodes before the first paint would undo the
// instant-open work. `rendered` is how many of `view` already exist on screen.
const PAGE = 160;
let rendered = 0;

const ICONS = {
  text: '<path d="M5 7h14M5 12h10M5 17h7" stroke-linecap="round"/>',
  rich: '<path d="M5 7h14M5 12h10M5 17h7" stroke-linecap="round"/>',
  files: '<path d="M4 7a2 2 0 0 1 2-2h3.6l1.6 2H18a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2z"/>',
};

function svg(paths, cls) {
  return `<svg class="${cls || ''}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7">${paths}</svg>`;
}

function timeAgo(ts) {
  const d = (Date.now() - ts) / 1000;
  if (d < 45) return '刚刚';
  if (d < 3600) return `${Math.floor(d / 60)} 分钟前`;
  if (d < 86400) return `${Math.floor(d / 3600)} 小时前`;
  if (d < 172800) return '昨天';
  const dt = new Date(ts);
  const p = (n) => String(n).padStart(2, '0');
  return `${p(dt.getMonth() + 1)}-${p(dt.getDate())}`;
}

function baseName(p) {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

function humanSize(n) {
  if (!n) return '';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function score(item, q) {
  if (!q) return 1;
  const hay = (item.text || '').toLowerCase();
  if (hay.includes(q)) return 100;
  let i = 0;
  for (const ch of hay) {
    if (ch === q[i]) i++;
    if (i === q.length) return 50;
  }
  return 0;
}

function kindOf(item) {
  if (item.kind === 'image') return 'image';
  if (item.kind === 'files') return 'files';
  return 'text';
}

function sigOf(items) {
  let s = String(items.length);
  for (const it of items) s += it.id + (it.pinned ? '1' : '0');
  return s;
}

function refresh() {
  const q = qEl.value.trim().toLowerCase();
  const key = `${q}\u0000${scope}\u0000${allSig}`;
  if (built && key === viewKey) {
    // Same data and same filter as what is already on screen — just move the
    // highlight. This is what makes reopening the popup feel instant.
    applySelection();
    return;
  }
  viewKey = key;

  view = all
    .filter((it) => (scope === 'all' ? true : kindOf(it) === scope))
    .map((it) => ({ it, s: score(it, q) }))
    .filter((x) => x.s > 0)
    .sort((a, b) => {
      if (a.it.pinned !== b.it.pinned) return a.it.pinned ? -1 : 1;
      if (b.s !== a.s) return b.s - a.s;
      return b.it.created_at - a.it.created_at;
    })
    .map((x) => x.it);

  if (sel >= view.length) sel = Math.max(0, view.length - 1);
  render();
  built = true;
}

function applySelection() {
  const rows = listEl.children;
  for (let i = 0; i < rows.length; i++) rows[i].classList.toggle('sel', i === sel);
  scrollToSel();
}

function makeRow(it, idx) {
  const row = document.createElement('div');
  row.className = 'row' + (idx === sel ? ' sel' : '') + (it.pinned ? ' pinned' : '');
  if (it.kind !== 'image') row.classList.add('single');

  // icon / thumbnail
  if (it.kind === 'image' && it.thumb) {
    const img = document.createElement('img');
    img.className = 'thumb-big';
    img.src = 'data:image/png;base64,' + it.thumb;
    row.appendChild(img);
  } else {
    const ico = document.createElement('div');
    ico.className = 'ico';
    ico.innerHTML = svg(ICONS[kindOf(it)] || ICONS.text);
    row.appendChild(ico);
  }

  const body = document.createElement('div');
  body.className = 'body';

  const text = document.createElement('div');
  text.className = 'text';
  if (it.kind === 'files') {
    text.textContent = (it.files || []).map(baseName).join('  ·  ');
  } else if (it.kind === 'image') {
    text.textContent = '图片';
  } else {
    text.textContent = (it.text || '').replace(/\s+/g, ' ').trim() || '(空)';
  }
  body.appendChild(text);

  const meta = document.createElement('div');
  meta.className = 'meta';
  const bits = [
    { text: timeAgo(it.created_at) },
    { text: it.kind === 'rich' ? '富文本' : it.kind === 'image' ? '图片' : it.kind === 'files' ? `${(it.files || []).length} 个文件` : '文本' },
  ];
  if (it.kind === 'image') bits.push({ text: humanSize(it.size) });
  else if (it.kind !== 'files') bits.push({ text: `${(it.text || '').length} 字符` });

  bits.forEach((b, i) => {
    if (i) {
      const dot = document.createElement('span');
      dot.className = 'dot';
      dot.textContent = '·';
      meta.appendChild(dot);
    }
    const s = document.createElement('span');
    s.textContent = b.text;
    meta.appendChild(s);
  });
  body.appendChild(meta);
  row.appendChild(body);

  const pin = document.createElement('div');
  pin.className = 'pin';
  pin.innerHTML = svg('<path d="M12 17v4"/><path d="M9 3h6l-1 5 3 3H7l3-3z" stroke-linejoin="round"/>');
  row.appendChild(pin);

  row.addEventListener('mousedown', (e) => {
    e.preventDefault();
    sel = idx;
    applySelection();
    choose(e.altKey);
  });

  return row;
}

function appendRows(upto) {
  if (upto <= rendered) return;
  const frag = document.createDocumentFragment();
  for (let i = rendered; i < upto; i++) frag.appendChild(makeRow(view[i], i));
  listEl.appendChild(frag);
  rendered = upto;
}

// `sel` can run past what has been built — keep the target row plus one page
// of slack in the DOM so scrolling stays continuous.
function ensureRow(idx) {
  if (idx < rendered) return;
  appendRows(Math.min(view.length, Math.max(idx + 1, rendered + PAGE)));
}

function render() {
  listEl.textContent = '';
  rendered = 0;
  appendRows(Math.min(view.length, PAGE));
  ensureRow(sel);

  const emptyOn = view.length === 0;
  emptyEl.classList.toggle('on', emptyOn);
  listEl.style.display = emptyOn ? 'none' : 'block';
  if (emptyOn) {
    const t = all.length === 0 ? '还没有历史' : '没有匹配的结果';
    emptyEl.querySelector('.empty-title').textContent = t;
    emptyEl.querySelector('.empty-sub').textContent =
      all.length === 0 ? '复制任何文本、图片或文件，它们会出现在这里' : '换个关键词试试';
  }

  countEl.textContent = view.length ? `${view.length} 项` : '';
  scrollToSel();
}

function scrollToSel() {
  const row = listEl.children[sel];
  if (row) row.scrollIntoView({ block: 'nearest' });
}

// Infinite feed: the next page is only materialised when the user gets close
// to the bottom, so a 20k-entry history still opens in one frame.
listEl.addEventListener('scroll', () => {
  if (rendered >= view.length) return;
  if (listEl.scrollTop + listEl.clientHeight >= listEl.scrollHeight - 240) {
    appendRows(Math.min(view.length, rendered + PAGE));
  }
});

function move(delta) {
  if (!view.length) return;
  sel = Math.min(view.length - 1, Math.max(0, sel + delta));
  ensureRow(sel);
  // Only the highlight changes — swapping a class beats rebuilding the list.
  applySelection();
}

async function choose(paste) {
  const it = view[sel];
  if (!it) return;
  try {
    await invoke('copy_item', { id: it.id, plain: false, paste: !!paste });
  } catch (err) {
    console.error(err);
  }
}

async function plainPaste() {
  const it = view[sel];
  if (!it) return;
  try {
    await invoke('copy_item', { id: it.id, plain: true, paste: true });
  } catch (err) {
    console.error(err);
  }
}

async function removeSel() {
  const it = view[sel];
  if (!it) return;
  await invoke('delete_item', { id: it.id });
  all = all.filter((x) => x.id !== it.id);
  allSig = sigOf(all);
  refresh();
}

async function pinSel() {
  const it = view[sel];
  if (!it) return;
  const pinned = await invoke('toggle_pin', { id: it.id });
  const target = all.find((x) => x.id === it.id);
  if (target) target.pinned = pinned;
  allSig = sigOf(all);
  refresh();
}

function cycleScope() {
  scope = SCOPES[(SCOPES.indexOf(scope) + 1) % SCOPES.length];
  scopeEl.value = scope;
  scopeEl.dataset.scope = scope;
  sel = 0;
  refresh();
}

qEl.addEventListener('input', () => {
  sel = 0;
  refresh();
});

// The select is the source of truth for the filter; Tab still cycles it.
scopeEl.addEventListener('change', () => {
  scope = scopeEl.value;
  scopeEl.dataset.scope = scope;
  sel = 0;
  refresh();
  if (document.activeElement === scopeEl) scopeEl.blur();
  qEl.focus();
});
scopeEl.dataset.scope = scope;

document.addEventListener('mousedown', (e) => {
  if (!e.target.closest('.row')) {
    qEl.focus();
  }
});

document.addEventListener('keydown', (e) => {
  if (e.isComposing || e.keyCode === 229) return;

  switch (e.key) {
    case 'Escape':
      e.preventDefault();
      invoke('hide_popup_cmd');
      return;
    case 'ArrowDown':
      e.preventDefault();
      move(1);
      return;
    case 'ArrowUp':
      e.preventDefault();
      move(-1);
      return;
    case 'PageDown':
      e.preventDefault();
      move(6);
      return;
    case 'PageUp':
      e.preventDefault();
      move(-6);
      return;
    case 'Tab':
      e.preventDefault();
      cycleScope();
      return;
    case 'Enter':
      e.preventDefault();
      if (e.altKey && e.shiftKey) plainPaste();
      else if (e.altKey) choose(true);
      else choose(false);
      return;
    case 'Delete':
      e.preventDefault();
      removeSel();
      return;
    case 'Backspace':
      if (e.ctrlKey) {
        e.preventDefault();
        removeSel();
      }
      return;
    default:
      break;
  }

  const k = e.key.toLowerCase();
  if (e.ctrlKey && k === 'p') {
    e.preventDefault();
    pinSel();
  } else if (e.ctrlKey && k === 'n') {
    e.preventDefault();
    move(1);
  } else if (e.ctrlKey && k === ',') {
    e.preventDefault();
    invoke('show_settings');
  }
});

// Pulls a fresh copy from the backend. Never on the critical path: the page
// renders from its in-memory cache first and only re-renders if something
// actually changed since the last sync.
let syncing = false;
async function syncFromBackend() {
  if (syncing) return;
  syncing = true;
  try {
    const next = await invoke('list_items');
    const sig = sigOf(next);
    if (sig === allSig) return;
    all = next;
    allSig = sig;
    refresh();
  } catch (err) {
    console.error(err);
  } finally {
    syncing = false;
  }
}

function resetView() {
  qEl.value = '';
  scope = 'all';
  scopeEl.value = 'all';
  scopeEl.dataset.scope = 'all';
  sel = 0;
}

// Everything below is synchronous on purpose: the handler runs while the window
// is still hidden, and the window is only shown once this returns, so the very
// first painted frame already shows the right list. Anything async here (an
// await on an IPC call, a requestAnimationFrame) would put a blank or stale
// frame on screen first.
listen('popup-shown', (e) => {
  // Theme rides along with the event, so the accent is always the current one
  // on the very first painted frame — no extra IPC, no stale colour.
  const p = e && e.payload;
  if (p && p.accent) window.__teecyTheme.applyAll(p.theme, p.accent);
  else window.__teecyTheme.refresh();
  resetView();
  refresh();
  qEl.focus();
  // The cache is kept current by `history-changed`, so this is only a safety
  // net — pushed past the first frame so it can never delay it.
  setTimeout(syncFromBackend, 120);
});

// Fired once the window has actually become active; makes sure the caret is in
// the search box even if the earlier focus() was ignored by a hidden window.
listen('popup-active', () => {
  qEl.focus();
});

listen('history-changed', () => {
  syncFromBackend();
});

// Fired instead of closing the window when "keep on screen" is enabled.
listen('item-copied', () => {
  const row = listEl.children[sel];
  if (!row) return;
  row.classList.remove('copied');
  // Force a reflow so a rapid second copy restarts the animation.
  void row.offsetWidth;
  row.classList.add('copied');
  setTimeout(() => row.classList.remove('copied'), 480);
});

// The settings window mirrors the theme into localStorage, which already
// updates the popup through the `storage` event; this covers the case where
// that write happened before this document existed.
listen('settings-changed', () => {
  invoke('get_settings')
    .then((s) => window.__teecyTheme.applyAll(s.theme, s.accent))
    .catch(() => {});
});

function boot() {
  invoke('get_settings')
    .then((s) => window.__teecyTheme.applyAll(s.theme, s.accent))
    .catch(() => {});
  invoke('list_items')
    .then((items) => {
      all = items;
      allSig = sigOf(all);
      refresh();
    })
    .catch(console.error)
    .finally(() => qEl.focus());
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot);
} else {
  boot();
}
