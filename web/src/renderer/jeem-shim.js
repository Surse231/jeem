// Replaces the Electron preload: window.electronAPI is backed by the Rust server (/__ipc/<command>).
(() => {
  const J = window.__JEEM__ || {};
  const base = location.origin;
  const headers = { 'Content-Type': 'application/json', 'X-Jeem-Token': J.token || '' };
  const listeners = {};
  let zoom = 1;

  const call = (cmd, args) =>
    fetch(`${base}/__ipc/${cmd}`, { method: 'POST', headers, body: JSON.stringify({ args: args || [] }) })
      .then((r) => r.json())
      .then((j) => (j ? j.result : null))
      .catch(() => null);

  const sync = (cmd, args) => {
    try {
      const x = new XMLHttpRequest();
      x.open('POST', `${base}/__ipc/${cmd}`, false);
      Object.entries(headers).forEach(([k, v]) => x.setRequestHeader(k, v));
      x.send(JSON.stringify({ args: args || [] }));
      return JSON.parse(x.responseText).result;
    } catch (_) {
      return null;
    }
  };

  const own = {
    serverToken: J.token || '',
    isWindows: true,
    platform: 'win32',
    getServerPort: async () => Number(location.port),
    storageInitSync: () => sync('storageInitSync'),
    storageSaveSync: (data) => sync('storageSaveSync', [data]),
    setZoomFactor: (f) => {
      zoom = Number(f) || 1;
      document.documentElement.style.zoom = String(zoom);
    },
    getZoomFactor: () => zoom,
    clearWebFrameCache: () => {},
    onWindowVisibility: (cb) => document.addEventListener('visibilitychange', () => cb(!document.hidden)),
    onWelcomeResult: (cb) => {
      call('takeWelcomeResult').then((r) => r && cb(r));
    },
  };

  window.__jeemEmit = (name, data) => (listeners[name] || []).forEach((cb) => { try { cb(data); } catch (_) {} });

  window.electronAPI = new Proxy(own, {
    get(target, prop) {
      if (prop in target) return target[prop];
      if (typeof prop !== 'string') return undefined;
      if (/^on[A-Z]/.test(prop)) {
        return (cb) => {
          (listeners[prop] = listeners[prop] || []).push(cb);
        };
      }
      return (...args) => call(prop, args);
    },
    has: () => true,
  });

  // The interface runs in a web view; hide what would make it feel like a browser page.
  const isEditable = (t) => !!(t && t.closest && t.closest('input, textarea, [contenteditable="true"]'));
  window.addEventListener('contextmenu', (e) => {
    if (!e.defaultPrevented && !isEditable(e.target)) e.preventDefault();
  });
  window.addEventListener('keydown', (e) => {
    const k = (e.key || '').toLowerCase();
    const ctrl = e.ctrlKey || e.metaKey;
    const browserKey =
      k === 'f5' || k === 'f3' || k === 'f7' ||
      (ctrl && ['r', 'p', 's', 'u', 'g', 'j', 'h'].includes(k)) ||
      (ctrl && !isEditable(e.target) && ['=', '+', '-', '0'].includes(k));
    if (browserKey) e.preventDefault();
  }, true);
  window.addEventListener('wheel', (e) => { if (e.ctrlKey) e.preventDefault(); }, { passive: false, capture: true });
  window.addEventListener('dragstart', (e) => {
    if (e.target && e.target.closest && e.target.closest('img, a') && !e.target.closest('[draggable="true"]')) e.preventDefault();
  });

  // Frameless window: elements marked `-webkit-app-region: drag` move the window.
  document.addEventListener('mousedown', (e) => {
    if (e.button !== 0) return;
    const region = getComputedStyle(e.target).getPropertyValue('-webkit-app-region');
    if (region === 'drag') call(e.detail === 2 ? 'maximizeWindow' : 'windowDrag');
  });
})();
