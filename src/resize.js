// Frameless windows have no system chrome to grab, so this adds invisible
// grips along every edge and corner that start a native resize loop. If the
// window already handles the edge through its native hit test the DOM never
// sees the pointer there, so the two paths never fight.
(function () {
  var DIRS = [
    ['n', 'North'], ['s', 'South'], ['w', 'West'], ['e', 'East'],
    ['nw', 'NorthWest'], ['ne', 'NorthEast'], ['sw', 'SouthWest'], ['se', 'SouthEast']
  ];

  function boot() {
    var api = window.__TAURI__ && window.__TAURI__.window;
    if (!api || !api.getCurrentWindow) return;
    var win = api.getCurrentWindow();

    var frag = document.createDocumentFragment();
    DIRS.forEach(function (d) {
      var el = document.createElement('div');
      el.className = 'rz rz-' + d[0];
      el.addEventListener('mousedown', function (e) {
        if (e.button !== 0) return;
        e.preventDefault();
        e.stopPropagation();
        try {
          win.startResizeDragging(d[1]);
        } catch (err) {
          // Older core without resize dragging — native borders still work.
        }
      });
      frag.appendChild(el);
    });
    document.body.appendChild(frag);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', boot);
  } else {
    boot();
  }
})();
