// Russian interface: translates every visible text, tooltip (title), aria-label and placeholder on the page,
// including elements created later. Original English text is remembered, so switching language restores it.
(() => {
  'use strict';

  const lang = () => localStorage.getItem('jeem_ui_lang') || 'en';
  const dict = () => (window.BUILTIN_UI_DICT && window.BUILTIN_UI_DICT.ru) || window.JEEM_RU_DICT || {};

  const plural = (n, one, few, many) => {
    const a = n % 10;
    const b = n % 100;
    if (a === 1 && b !== 11) return one;
    if (a >= 2 && a <= 4 && (b < 10 || b >= 20)) return few;
    return many;
  };
  const duration = (r) => (r || '').replace(/(\d+) hr/, '$1 ч').replace(/(\d+) min/, '$1 мин');
  const noun = (word, one, few, many) => [new RegExp('^(\\d+) ' + word + '$'), (n) => `${n} ${plural(+n, one, few, many)}`];
  const count = (one, few, many) => (n) => `${n} ${/^\d+$/.test(n) ? plural(+n, one, few, many) : many}`;

  const PATTERNS = [
    noun('songs?', 'песня', 'песни', 'песен'),
    noun('tracks?', 'трек', 'трека', 'треков'),
    noun('playlists?', 'плейлист', 'плейлиста', 'плейлистов'),
    noun('albums?', 'альбом', 'альбома', 'альбомов'),
    noun('artists?', 'артист', 'артиста', 'артистов'),
    noun('files?', 'файл', 'файла', 'файлов'),
    noun('lines?', 'строка', 'строки', 'строк'),
    noun('followers?', 'подписчик', 'подписчика', 'подписчиков'),
    [/^(\d+) selected$/, (n) => `Выбрано: ${n}`],
    [/^(\d+) min$/, (n) => `${n} мин`],
    [/^(\d+) sec$/, (n) => `${n} с`],
    [/^(\d+) \/ (\d+) lines synced$/, (a, b) => `Синхронизировано строк: ${a} / ${b}`],
    [/^(\d+) tracks? \((.+)\)$/, (n, size) => `${n} ${plural(+n, 'трек', 'трека', 'треков')} (${size.replace(/\bB\b/, 'Б').replace(/KB/, 'КБ').replace(/MB/, 'МБ').replace(/GB/, 'ГБ')})`],
    [/^(\d+(?:\.\d+)?) (KB|MB|GB)$/, (n, u) => `${n} ${{ KB: 'КБ', MB: 'МБ', GB: 'ГБ' }[u]}`],
    [/^Version (.+)$/, (v) => `Версия ${v}`],
        [/^Added to (.+)$/, (x) => `Добавлено в «${x.replace(/^"|"$/g, '')}»`],
    [/^Removed from (.+)$/, (x) => `Удалено из «${x.replace(/^"|"$/g, '')}»`],
    [/^Saved to (.+)$/, (x) => `Сохранено в ${x}`],
    [/^Downloading (.+)$/, (x) => `Загрузка: ${x}`],
    [/^Imported (\d+) tracks?$/, (n) => `Импортировано треков: ${n}`],
    [/^Search results for "(.+)"$/, (q) => `Результаты поиска по запросу «${q}»`],
    [/^(\d+) tracks not found$/, (n) => `Не найдено треков: ${n}`],
    [/^Removed (\d+) tracks?$/, (n) => `Удалено треков: ${n}`],
    [/^Added (\d+) tracks?$/, (n) => `Добавлено треков: ${n}`],
    [/^Added (\d+) tracks? to "(.+)"$/, (n, p) => `Добавлено треков: ${n} в «${p}»`],
    [/^Added (\d+) tracks? to favorites$/, (n) => `Добавлено в избранное: ${n}`],
    [/^Sleep timer: (\d+) min$/, (n) => `Таймер сна: ${n} мин`],
    [/^Added (\d+) themes?\. Turn one on to try it$/, (n) => `Добавлено тем: ${n}. Включите одну, чтобы попробовать`],
    [/^Added (\d+) plugins?\. Turn one on to run it$/, (n) => `Добавлено плагинов: ${n}. Включите один, чтобы запустить`],
    [/^Plugin (.+) failed: (.+)$/, (f, e) => `Плагин ${f} дал сбой: ${e}`],
    [/^Restart Jeem to fully unload "(.+)"$/, (p) => `Перезапустите Jeem, чтобы полностью выгрузить «${p}»`],
    [/^Exported (\d+) playlists?$/, (n) => `Экспортировано плейлистов: ${n}`],
    [/^Imported (\d+) playlists? \((\d+) tracks\)$/, (n, m) => `Импортировано плейлистов: ${n} (треков: ${m})`],
    [/^Downloading: (.+) \((\..+)\)$/, (t, f) => `Загрузка: ${t} (${f})`],
    [/^Downloading (\d+)\/(\d+)$/, (a, b) => `Загрузка ${a} из ${b}`],
    [/^Downloaded (\d+)\/(\d+) \((\d+) failed\)$/, (a, b, c) => `Скачано ${a} из ${b} (ошибок: ${c})`],
    [/^Downloaded (\d+) tracks?( to .+)?$/, (n, d) => `Скачано треков: ${n}${d ? ' в' + d.slice(3) : ''}`],
    [/^Download (\d+) tracks?$/, (n) => `Скачать треков: ${n}`],
    [/^Found (\d+) liked songs and (\d+) playlists$/, (a, b) => `Найдено любимых песен: ${a} и плейлистов: ${b}`],
    [/^Found (\d+) liked tracks$/, (n) => `Найдено любимых треков: ${n}`],
    [/^Found (\d+) playlists \((\d+) tracks\)$/, (a, b) => `Найдено плейлистов: ${a} (треков: ${b})`],
    [/^Blocked track "(.+)"$/, (t) => `Трек «${t}» заблокирован`],
    [/^Click to unblock (.+)$/, (t) => `Нажмите, чтобы разблокировать: ${t}`],
    [/^Delete playlist "(.+)"\? This action cannot be undone\.$/, (p) => `Удалить плейлист «${p}»? Это действие нельзя отменить.`],
    [/^(.+) • buffering stream$/, (a) => `${a} • буферизация потока`],
    [/^(\d+) of (\d+) tracks$/, (a, b) => `${a} из ${b} треков`],
    [/^(\d+) of (\d+) tracks in library$/, (a, b) => `${a} из ${b} треков в библиотеке`],
    [/^(\d+) tracks in library$/, (n) => `${n} ${plural(+n, 'трек', 'трека', 'треков')} в библиотеке`],
    [/^(.+) • (\d+) of (\d+) songs(.*)$/, (u, a, b, r) => `${u} • ${a} из ${b} песен${duration(r)}`],
    [/^(.+) • (\d+) songs?(.*)$/, (u, n, r) => `${u} • ${n} ${plural(+n, 'песня', 'песни', 'песен')}${duration(r)}`],
    [/^Artist • ([\d.,]+[KMB]?) listeners$/, (n) => `Артист • слушателей: ${n}`],
    [/^([\d.,]+[KMB]?) listeners$/, count('слушатель', 'слушателя', 'слушателей')],
    [/^Artist • ([\d.,\s\u00a0]+[KMB]?) fans$/, (n) => `Артист • фанатов: ${n}`],
    [/^([\d.,\s\u00a0]+[KMB]?) fans$/, (n) => `Фанатов: ${n}`],
    [/^Artist • ([\d.,]+[KMB]?) monthly listeners$/, (n) => `Артист • слушателей за месяц: ${n}`],
    [/^([\d.,]+[KMB]?) monthly listeners$/, (n) => `Слушателей за месяц: ${n}`],
    [/^([\d.,]+[KMB]?) plays$/, (n) => `Прослушиваний: ${n}`],
    [/^([\d.,]+[KMB]?) plays?$/, count('прослушивание', 'прослушивания', 'прослушиваний')],
    [/^(\d+)m ago$/, (n) => `${n} мин назад`],
    [/^(\d+)h ago$/, (n) => `${n} ч назад`],
    [/^(\d+)d ago$/, (n) => `${n} дн. назад`],
    [/^(\d+)mo ago$/, (n) => `${n} мес. назад`],
    [/^(.+) \(default\)$/, (p) => `${p} (по умолчанию)`],
    [/^By (.+)$/, (a) => `От ${a}`, true],
    [/^View (.+)$/, (a) => `Открыть: ${a}`, true],
    [/^Open (.+)$/, (a) => `Открыть: ${a}`, true],
    [/^Play (.+)$/, (a) => `Играть: ${a}`, true],
    [/^Pause (.+)$/, (a) => `Пауза: ${a}`, true],
    [/^Remove (.+) from (.+)$/, (a, b) => `Убрать «${a}» из «${b}»`, true],
    [/^Add (.+) to (.+)$/, (a, b) => `Добавить «${a}» в «${b}»`, true],
  ];

  let prefixes = null;
  let prefixSource = null;

  function prefixList() {
    const d = dict();
    if (prefixSource !== d) {
      prefixSource = d;
      prefixes = Object.keys(d)
        .filter((k) => /(:| \(| @| to)$/.test(k))
        .sort((a, b) => b.length - a.length);
    }
    return prefixes;
  }

  // Returns the Russian text for an English string, or null when there is no translation.
  function translate(text, attrOnly) {
    if (typeof text !== 'string') return null;
    const key = text.replace(/\s+/g, ' ').trim();
    if (!key || !/[A-Za-z]{2}/.test(key)) return null;
    const d = dict();
    let out = null;
    if (Object.prototype.hasOwnProperty.call(d, key)) out = d[key];
    else {
      for (const [re, fn, onlyAttr] of PATTERNS) {
        if (onlyAttr && !attrOnly) continue;
        const m = key.match(re);
        if (m) {
          out = fn(...m.slice(1));
          break;
        }
      }
      if (out === null) {
        for (const p of prefixList()) {
          if (key.length > p.length && key.startsWith(p)) {
            out = d[p] + key.slice(p.length);
            break;
          }
        }
      }
    }
    if (out === null && / [·•] /.test(key)) {
      let changed = false;
      const parts = key.split(/( [·•] )/).map((part) => {
        if (/^ [·•] $/.test(part)) return part;
        const t = translate(part, attrOnly);
        if (t !== null) changed = true;
        return t === null ? part : t;
      });
      if (changed) out = parts.join('');
    }
    if (out === null || out === key) return null;
    const lead = text.match(/^\s*/)[0];
    const trail = text.match(/\s*$/)[0];
    return lead + out + trail;
  }
  window.__jeemTranslate = translate;

  const EXCLUDE = [
    '#ui-lang-dropdown', '#tr-lang-dropdown', '#langMenu', 'script', 'style', 'textarea', '[contenteditable="true"]',
    '[data-i18n-fixed]', '[translate="no"]', '[class*="material-symbols"]', '[class*="material-icons"]', '.track-row', '.sp-card', '.sb-lib-row', '.np-queue-row', '#lyrics-container',
    '#playbar-title', '#playbar-artist', '#right-panel-title', '#right-panel-artist', '.right-panel-meta',
    '#playlist-detail-title', '#playlist-detail-desc', '#album-detail-title', '#album-detail-artist',
    '#artist-profile-name', '#artist-profile-bio', '.tv-karaoke-meta',
  ].join(',');
  // Tooltips are interface text even inside track rows and cards, so only a few places are skipped.
  const ATTR_EXCLUDE = 'script, style, [translate="no"]';
  // Small caption lines ("By Name", "Album · 2013") may use the caption patterns too.
  const SUB_SEL = '[class*="-sub"], [class$="sub"], [class*="subtitle"]';
  const ATTRS = ['title', 'aria-label', 'placeholder', 'alt'];

  const textOrig = new Map(); // text node -> { en, ru }
  const attrOrig = new Map(); // element -> { attr: { en, ru } }

  function excluded(node) {
    const el = node.nodeType === 1 ? node : node.parentElement;
    return !el || !!el.closest(EXCLUDE);
  }

  function doText(node) {
    const rec = textOrig.get(node);
    if (rec && node.data === rec.ru) return;
    if (excluded(node)) return;
    const parent = node.parentElement;
    const tr = translate(node.data, !!(parent && parent.matches(SUB_SEL)));
    if (tr === null) return;
    textOrig.set(node, { en: node.data, ru: tr });
    node.data = tr;
  }

  function doAttrs(el) {
    if (el.nodeType !== 1 || el.closest(ATTR_EXCLUDE)) return;
    for (const a of ATTRS) {
      if (!el.hasAttribute(a)) continue;
      const cur = el.getAttribute(a);
      let rec = attrOrig.get(el);
      if (rec && rec[a] && rec[a].ru === cur) continue;
      const tr = translate(cur, true);
      if (tr === null) continue;
      if (!rec) attrOrig.set(el, (rec = {}));
      rec[a] = { en: cur, ru: tr };
      el.setAttribute(a, tr);
    }
  }

  function walk(root) {
    if (!root) return;
    if (root.nodeType === 3) return doText(root);
    if (root.nodeType !== 1) return;
    doAttrs(root);
    const tw = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
    let n = tw.nextNode();
    while (n) {
      if (n.nodeType === 3) doText(n);
      else if (n.tagName !== 'SCRIPT' && n.tagName !== 'STYLE') doAttrs(n);
      n = tw.nextNode();
    }
  }

  function restoreEnglish() {
    for (const [node, rec] of textOrig) {
      if (node.isConnected && node.data === rec.ru) node.data = rec.en;
    }
    textOrig.clear();
    for (const [el, rec] of attrOrig) {
      for (const [a, v] of Object.entries(rec)) if (el.isConnected && el.getAttribute(a) === v.ru) el.setAttribute(a, v.en);
    }
    attrOrig.clear();
  }

  let pending = new Set();
  let scheduled = false;
  function flush() {
    scheduled = false;
    if (lang() !== 'ru') {
      pending.clear();
      return;
    }
    const batch = pending;
    pending = new Set();
    observer.disconnect();
    try {
      batch.forEach((n) => n.isConnected && walk(n));
    } finally {
      observe();
    }
  }
  function queue(n) {
    pending.add(n);
    if (!scheduled) {
      scheduled = true;
      requestAnimationFrame(flush);
    }
  }

  const observer = new MutationObserver((records) => {
    if (lang() !== 'ru') return;
    for (const r of records) {
      if (r.type === 'childList') r.addedNodes.forEach(queue);
      else queue(r.target);
    }
  });
  function observe() {
    observer.observe(document.body, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ATTRS });
  }

  let current = null;
  let dictSize = 0;
  let titleEn = null;
  function sync() {
    const l = lang();
    const size = Object.keys(dict()).length;
    if (l === current && size === dictSize) return;
    if (l === current && l !== 'ru') return;
    dictSize = size;
    observer.disconnect();
    try {
      if (l === 'ru') {
        walk(document.body);
        if (titleEn === null) titleEn = document.title;
        const tt = translate(titleEn);
        if (tt) document.title = tt.trim();
      } else {
        restoreEnglish();
        if (titleEn !== null) document.title = titleEn;
        titleEn = null;
      }
      current = l;
    } catch (e) {
      console.error('[i18n]', e);
    } finally {
      observe();
    }
  }
  window.__jeemI18n = { sync, walk, translate };

  function start() {
    sync();
    observe();
    setInterval(sync, 400);
  }
  if (document.body) start();
  else document.addEventListener('DOMContentLoaded', start);
})();
