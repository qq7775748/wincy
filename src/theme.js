// Theme plumbing shared by the popup and the settings window.
//
// The authoritative preference lives in Settings.theme on the Rust side. This
// file mirrors it to localStorage so a freshly shown popup can pick the right
// palette before it paints — an IPC round trip would land a frame too late and
// the window would flash the wrong colours on every hotkey press.
(function () {
  var KEY = 'wincy-theme';
  var AKEY = 'wincy-accent';
  var ACCENTS = ['blue', 'indigo', 'purple', 'pink', 'red', 'orange', 'green', 'graphite'];
  // Colour values live here as well as in style.css: the inline custom property
  // below outranks any selector, so a theme change can never lose to CSS
  // specificity (that is exactly how the first cut failed silently).
  var ACCENT_HEX = {
    blue: '#0a84ff',
    indigo: '#5b5bd6',
    purple: '#8b5cf6',
    pink: '#ec4899',
    red: '#e11d48',
    orange: '#e3630a',
    green: '#17883f',
    graphite: '#6b7280'
  };
  var root = document.documentElement;
  var mq = window.matchMedia('(prefers-color-scheme: dark)');

  function pref() {
    try {
      var v = localStorage.getItem(KEY);
      return v === 'light' || v === 'dark' || v === 'system' ? v : 'system';
    } catch (e) {
      return 'system';
    }
  }

  function accent() {
    try {
      var v = localStorage.getItem(AKEY);
      return ACCENTS.indexOf(v) >= 0 ? v : 'blue';
    } catch (e) {
      return 'blue';
    }
  }

  function apply(p) {
    if (p !== 'light' && p !== 'dark') p = 'system';
    var resolved = p === 'system' ? (mq.matches ? 'dark' : 'light') : p;
    if (root.dataset.theme !== resolved) root.dataset.theme = resolved;
    root.dataset.themePref = p;
    return resolved;
  }

  function applyAccent(a) {
    if (ACCENTS.indexOf(a) < 0) a = 'blue';
    if (root.dataset.accent !== a) root.dataset.accent = a;
    // Inline custom properties beat every stylesheet rule, including the
    // `:root[data-accent=...]` blocks in style.css.
    root.style.setProperty('--sel', ACCENT_HEX[a]);
    return a;
  }

  window.__wincyTheme = {
    ACCENTS: ACCENTS,
    pref: pref,
    accent: accent,
    apply: apply,
    applyAccent: applyAccent,
    // Called by the settings UI: persist locally, paint immediately, then let
    // the caller push the value to the backend.
    set: function (p) {
      try { localStorage.setItem(KEY, p); } catch (e) {}
      apply(p);
    },
    setAccent: function (a) {
      if (ACCENTS.indexOf(a) < 0) a = 'blue';
      try { localStorage.setItem(AKEY, a); } catch (e) {}
      applyAccent(a);
    },
    // Applies whatever the backend says, keeping localStorage in sync.
    applyAll: function (theme, ac) {
      window.__wincyTheme.set(theme || 'system');
      window.__wincyTheme.setAccent(ac || 'blue');
    },
    // Re-reads localStorage and repaints. Cheap and synchronous: used on every
    // popup show so a change made while the popup was hidden can never be
    // missed, whichever cross-window notification actually fired.
    refresh: function () {
      apply(pref());
      applyAccent(accent());
    }
  };

  apply(pref());
  applyAccent(accent());

  // "system" keeps tracking Windows' light/dark switch at runtime.
  var onSystemChange = function () { if (pref() === 'system') apply('system'); };
  if (mq.addEventListener) mq.addEventListener('change', onSystemChange);
  else if (mq.addListener) mq.addListener(onSystemChange);

  // Fires in the *other* window when the preference changes, which keeps the
  // popup in sync without any backend round trip.
  window.addEventListener('storage', function (e) {
    if (e.key === KEY) apply(e.newValue || 'system');
    else if (e.key === AKEY) applyAccent(e.newValue || 'blue');
  });
})();
