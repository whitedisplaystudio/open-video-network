// Shared helpers for both UIs.
//
// No framework and no build step: the node serves these files directly, so
// `cargo build` is still the whole toolchain. Everything is a module, and the
// Content-Security-Policy allows scripts only from this origin.

/** Call the local API. Auth is the cookie `/auth` set, so nothing to add. */
export async function api(path, options = {}) {
  const response = await fetch(path, {
    credentials: 'same-origin',
    ...options,
    headers: { Accept: 'application/json', ...(options.headers || {}) },
  });

  if (response.status === 401) {
    throw new ApiError(t('error.unauthorised'), 401);
  }
  const text = await response.text();
  let body = null;
  if (text) {
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
  }
  if (!response.ok) {
    const message = (body && body.error) || (typeof body === 'string' && body) || response.statusText;
    throw new ApiError(message, response.status);
  }
  return body;
}

export class ApiError extends Error {
  constructor(message, status) {
    super(message);
    this.status = status;
  }
}

export const get = (path) => api(path);
export const post = (path, body) =>
  api(path, {
    method: 'POST',
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
export const del = (path) => api(path, { method: 'DELETE' });

// ---------------------------------------------------------------- language

let strings = {};
let locale = 'en';
let plurals = new Intl.PluralRules('en');
let numbers = new Intl.NumberFormat('en');
let relative = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });
let dates = new Intl.DateTimeFormat('en', { dateStyle: 'medium' });
let dateTimes = new Intl.DateTimeFormat('en', { dateStyle: 'medium', timeStyle: 'short' });
const onLocaleChange = [];

const STORED_LOCALE = 'ovn.locale';

/**
 * Look up a translated string.
 *
 * Pass `{ count }` and the right plural form is chosen with the browser's
 * own CLDR data, so a language with six plural categories gets six and one
 * with none gets one. Any other values in `vars` fill `{placeholders}`.
 */
export function t(key, vars) {
  let value = strings[key];
  if (vars && typeof vars.count === 'number') {
    const category = plurals.select(vars.count);
    value = strings[`${key}_${category}`] ?? strings[`${key}_other`] ?? value;
  }
  if (value === undefined) return key;
  if (!vars) return value;
  return value.replace(/\{(\w+)\}/g, (whole, name) =>
    Object.hasOwn(vars, name) ? String(vars[name]) : whole,
  );
}

/** The packs this node can serve. */
export async function availableLocales() {
  const response = await fetch('/v1/locales', { credentials: 'same-origin' });
  if (!response.ok) return [];
  return response.json();
}

/**
 * Choose a language: what was picked here before, then what the browser
 * asks for, then English.
 */
function preferredLocale(available) {
  const codes = available.map((l) => l.locale);
  let stored = null;
  try {
    stored = localStorage.getItem(STORED_LOCALE);
  } catch {
    // Private windows and blocked storage: fall through to the browser's
    // own preference, which is a fine answer anyway.
  }
  if (stored && codes.includes(stored)) return stored;

  for (const wanted of navigator.languages ?? [navigator.language ?? 'en']) {
    const exact = codes.find((c) => c.toLowerCase() === wanted.toLowerCase());
    if (exact) return exact;
    const base = wanted.split('-')[0].toLowerCase();
    const loose = codes.find((c) => c.split('-')[0].toLowerCase() === base);
    if (loose) return loose;
  }
  return codes.includes('en') ? 'en' : codes[0];
}

/** Load a pack and apply it to the page. */
export async function setLocale(code, { remember = true } = {}) {
  const response = await fetch(`/v1/locales/${encodeURIComponent(code)}`, {
    credentials: 'same-origin',
  });
  if (!response.ok) return false;
  const pack = await response.json();

  strings = pack.strings;
  locale = pack.locale;
  plurals = new Intl.PluralRules(locale);
  numbers = new Intl.NumberFormat(locale);
  relative = new Intl.RelativeTimeFormat(locale, { numeric: 'auto' });
  dates = new Intl.DateTimeFormat(locale, { dateStyle: 'medium' });
  dateTimes = new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' });

  document.documentElement.lang = locale;
  // Arabic and Hebrew flip the whole layout; the stylesheet is written in
  // logical properties so this one attribute is the entire change.
  document.documentElement.dir = pack.direction ?? 'ltr';

  if (remember) {
    try {
      localStorage.setItem(STORED_LOCALE, locale);
    } catch {
      // A remembered language is a convenience, not a requirement.
    }
  }
  translate(document);
  for (const listener of onLocaleChange) listener();
  return true;
}

/** Run `fn` whenever the language changes, so views can re-render. */
export function whenLocaleChanges(fn) {
  onLocaleChange.push(fn);
}

export function currentLocale() {
  return locale;
}

/**
 * Apply translations to everything under `root` that asks for them.
 *
 * `data-i18n` sets the text; the others set an attribute. Only leaf elements
 * carry `data-i18n`, so nothing with children is ever emptied.
 */
export function translate(root = document) {
  for (const node of root.querySelectorAll('[data-i18n]')) {
    node.textContent = t(node.dataset.i18n);
  }
  for (const [selector, attribute, dataset] of [
    ['[data-i18n-placeholder]', 'placeholder', 'i18nPlaceholder'],
    ['[data-i18n-title]', 'title', 'i18nTitle'],
    ['[data-i18n-label]', 'aria-label', 'i18nLabel'],
  ]) {
    for (const node of root.querySelectorAll(selector)) {
      node.setAttribute(attribute, t(node.dataset[dataset]));
    }
  }
  const title = document.querySelector('title');
  if (title?.dataset.i18n) document.title = t(title.dataset.i18n);
}

/** Build the language picker and wire it up. */
export async function languagePicker(select) {
  const available = await availableLocales();
  if (!available.length) return;

  await setLocale(preferredLocale(available), { remember: false });

  for (const pack of available) {
    select.append(
      el('option', {
        value: pack.locale,
        // The language's own name, so you can find yours without reading
        // the one you do not speak.
        text: pack.name,
        selected: pack.locale === locale,
      }),
    );
  }
  select.value = locale;
  select.addEventListener('change', () => setLocale(select.value));
}

// ------------------------------------------------------------ formatting

/**
 * Binary sizes. The unit symbols are international; the number is formatted
 * for the current language, so Arabic gets Arabic-Indic digits and German
 * gets a comma.
 */
export function bytes(n) {
  if (!n) return `0 B`;
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const formatted =
    unit === 0
      ? numbers.format(n)
      : new Intl.NumberFormat(locale, { maximumFractionDigits: 1, minimumFractionDigits: 1 }).format(
          value,
        );
  return `${formatted} ${units[unit]}`;
}

/**
 * `h:mm:ss`, in Latin digits.
 *
 * Deliberately not localised: every video player in the world shows a
 * timeline this way, and mixing digit systems inside a timestamp reads
 * worse than leaving it alone.
 */
export function duration(secs) {
  secs = Math.max(0, Math.round(secs || 0));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  const pad = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

export function percent(fraction) {
  return new Intl.NumberFormat(locale, {
    style: 'percent',
    maximumFractionDigits: 0,
  }).format(fraction || 0);
}

export function number(n) {
  return numbers.format(n ?? 0);
}

export function decimal(n, digits = 2) {
  return new Intl.NumberFormat(locale, {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
    signDisplay: 'exceptZero',
  }).format(n ?? 0);
}

export function date(unixSeconds) {
  return dates.format(new Date((unixSeconds || 0) * 1000));
}

export function dateTime(unixSeconds) {
  return dateTimes.format(new Date((unixSeconds || 0) * 1000));
}

/**
 * "3 minutes ago", in whatever the current language says.
 *
 * `Intl.RelativeTimeFormat` carries the wording for every locale the browser
 * knows, so this needs no translated strings at all.
 */
export function ago(unixSeconds) {
  if (!unixSeconds) return t('admin.peers.never');
  const delta = Math.floor(Date.now() / 1000 - unixSeconds);
  const steps = [
    [60, 'second'],
    [3600, 'minute'],
    [86400, 'hour'],
    [604800, 'day'],
    [2629800, 'week'],
    [31557600, 'month'],
    [Infinity, 'year'],
  ];
  const divisors = [1, 60, 3600, 86400, 604800, 2629800, 31557600];
  for (let i = 0; i < steps.length; i += 1) {
    if (Math.abs(delta) < steps[i][0]) {
      return relative.format(-Math.round(delta / divisors[i]), steps[i][1]);
    }
  }
  return relative.format(-Math.round(delta / 31557600), 'year');
}

// -------------------------------------------------------------------- DOM

/** Build an element. Text is set as text, never as HTML. */
export function el(tag, attrs = {}, children = []) {
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {
    if (value === null || value === undefined || value === false) continue;
    if (key === 'class') node.className = value;
    else if (key === 'text') node.textContent = value;
    else if (key === 'html') throw new Error('refusing to set raw HTML');
    else if (key.startsWith('on')) node.addEventListener(key.slice(2).toLowerCase(), value);
    else if (key === 'dataset') Object.assign(node.dataset, value);
    else node.setAttribute(key, value === true ? '' : value);
  }
  for (const child of [].concat(children)) {
    if (child === null || child === undefined || child === false) continue;
    node.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return node;
}

export function clear(node) {
  while (node.firstChild) node.removeChild(node.firstChild);
  return node;
}

export function mount(node, children) {
  clear(node);
  for (const child of [].concat(children)) {
    if (child) node.append(child);
  }
  return node;
}

/** An empty-state block. `titleKey` and `hintKey` are translation keys. */
export function empty(icon, titleKey, hintKey) {
  return el('div', { class: 'empty' }, [
    el('div', { class: 'big', text: icon }),
    el('div', { text: t(titleKey) }),
    hintKey ? el('p', { class: 'lede', text: t(hintKey) }) : null,
  ]);
}

// ----------------------------------------------------------------- toasts

export function toast(message, kind = 'info') {
  const host = document.getElementById('toasts');
  if (!host) return;
  const node = el('div', { class: kind === 'error' ? 'toast error' : 'toast', text: message });
  host.append(node);
  setTimeout(() => node.remove(), kind === 'error' ? 8000 : 4000);
}

export function reportError(error) {
  console.error(error);
  toast(error instanceof Error ? error.message : String(error), 'error');
}

// ------------------------------------------------------------ live events

/**
 * Subscribe to the node's server-sent events.
 *
 * `handlers` maps an event `type` to a function; `'*'` receives everything.
 * The browser reconnects on its own, so a restarted node reattaches.
 */
export function liveEvents(handlers = {}) {
  const source = new EventSource('/v1/events');
  source.onmessage = (message) => {
    let event;
    try {
      event = JSON.parse(message.data);
    } catch {
      return;
    }
    handlers['*']?.(event);
    handlers[event.type]?.(event);
  };
  source.onerror = () => handlers.disconnected?.();
  source.onopen = () => handlers.connected?.();
  return source;
}

/** Poll `fn` every `ms`, and immediately. Returns a stop function. */
export function poll(fn, ms) {
  let stopped = false;
  const tick = async () => {
    if (stopped) return;
    try {
      await fn();
    } catch (error) {
      if (error.status !== 401) console.warn(error);
    }
  };
  tick();
  const handle = setInterval(tick, ms);
  return () => {
    stopped = true;
    clearInterval(handle);
  };
}

/** Wire up a `.nav` whose buttons carry `data-page`, and the matching pages. */
export function router(onChange) {
  const show = () => {
    const page = location.hash.replace(/^#\/?/, '').split('/')[0] || 'home';
    const arg = location.hash.replace(/^#\/?/, '').split('/').slice(1).join('/');
    for (const section of document.querySelectorAll('.page')) {
      section.hidden = section.dataset.page !== page;
    }
    for (const button of document.querySelectorAll('.nav button')) {
      if (button.dataset.page === page) button.setAttribute('aria-current', 'page');
      else button.removeAttribute('aria-current');
    }
    onChange(page, decodeURIComponent(arg));
  };
  for (const button of document.querySelectorAll('.nav button')) {
    button.addEventListener('click', () => {
      location.hash = `#/${button.dataset.page}`;
    });
  }
  window.addEventListener('hashchange', show);
  show();
  return show;
}

export function go(page, arg) {
  location.hash = arg ? `#/${page}/${encodeURIComponent(arg)}` : `#/${page}`;
}
