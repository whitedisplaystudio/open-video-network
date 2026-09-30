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

import { detectCountry } from '/assets/zones.js';

let strings = {};
let locale = 'en';          // the pack in use
let formatLocale = 'en';    // the full tag Intl formats with
let plurals = new Intl.PluralRules('en');
let numbers = new Intl.NumberFormat('en');
let relative = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });
let dates = new Intl.DateTimeFormat('en', { dateStyle: 'medium' });
let dateTimes = new Intl.DateTimeFormat('en', { dateStyle: 'medium', timeStyle: 'short' });
const onLocaleChange = [];

const STORED_LOCALE = 'ovn.locale';
const AUTOMATIC = 'auto';

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

/** The packs this node can serve, plus what it would choose itself. */
export async function localeCatalogue() {
  const response = await fetch('/v1/locales', { credentials: 'same-origin' });
  if (!response.ok) return { locales: [], configured: null, suggested: null };
  return response.json();
}

function readStored() {
  try {
    return localStorage.getItem(STORED_LOCALE);
  } catch {
    // Private windows and blocked storage: fall through to detection, which
    // is a fine answer anyway.
    return null;
  }
}

function matchPack(packs, tag) {
  const wanted = tag.toLowerCase();
  const exact = packs.find((p) => p.locale.toLowerCase() === wanted);
  if (exact) return exact;
  const base = wanted.split('-')[0];
  return packs.find((p) => p.locale.toLowerCase().split('-')[0] === base) ?? null;
}

/**
 * Work out which language to show, from local signals only.
 *
 * In order:
 *
 *   1. what was chosen here before;
 *   2. what the operator set on the node, if anything;
 *   3. what the browser asks for — an explicit preference beats a guess;
 *   4. where the browser's time zone says it is;
 *   5. the language this machine's operating system is set to;
 *   6. English.
 *
 * Step 4 is the only inference, and it is last among the automatic ones for
 * a reason: someone reading Japanese in Frankfurt should not be handed
 * German. It is what helps the case that actually matters — a browser set to
 * English on a device in São Paulo.
 *
 * Nothing here contacts anything. There is no IP lookup, and there will not
 * be one: it would mean telling a third party both where you are and that
 * you are running this.
 */
function resolveLocale(catalogue) {
  const packs = catalogue.locales ?? [];
  if (!packs.length) return { pack: 'en', format: 'en' };

  const stored = readStored();
  if (stored && stored !== AUTOMATIC) {
    const chosen = matchPack(packs, stored);
    if (chosen) return { pack: chosen.locale, format: stored };
  }

  if (catalogue.configured) {
    const chosen = matchPack(packs, catalogue.configured);
    if (chosen) return { pack: chosen.locale, format: catalogue.configured };
  }

  for (const wanted of navigator.languages ?? [navigator.language ?? 'en']) {
    const chosen = matchPack(packs, wanted);
    if (chosen) return { pack: chosen.locale, format: wanted };
  }

  const country = detectCountry();
  if (country) {
    const chosen = packs.find((p) => (p.regions ?? []).includes(country));
    // The region also sharpens the formatting: Brazil and Portugal share a
    // pack but not a date format.
    if (chosen) return { pack: chosen.locale, format: `${chosen.locale}-${country}` };
  }

  if (catalogue.suggested) {
    const chosen = matchPack(packs, catalogue.suggested);
    if (chosen) return { pack: chosen.locale, format: catalogue.suggested };
  }

  const fallback = packs.find((p) => p.locale === 'en') ?? packs[0];
  return { pack: fallback.locale, format: fallback.locale };
}

/** Load a pack and apply it to the page. */
export async function setLocale(code, { remember = true, format = null } = {}) {
  const response = await fetch(`/v1/locales/${encodeURIComponent(code)}`, {
    credentials: 'same-origin',
  });
  if (!response.ok) return false;
  const pack = await response.json();

  strings = pack.strings;
  locale = pack.locale;
  formatLocale = format ?? pack.locale;

  // Intl will reject a malformed tag; fall back to the pack rather than
  // breaking every date on the page.
  const intlFor = (options) => {
    try {
      return new Intl.DateTimeFormat(formatLocale, options);
    } catch {
      formatLocale = pack.locale;
      return new Intl.DateTimeFormat(pack.locale, options);
    }
  };
  dates = intlFor({ dateStyle: 'medium' });
  dateTimes = intlFor({ dateStyle: 'medium', timeStyle: 'short' });
  plurals = new Intl.PluralRules(formatLocale);
  numbers = new Intl.NumberFormat(formatLocale);
  relative = new Intl.RelativeTimeFormat(formatLocale, { numeric: 'auto' });

  document.documentElement.lang = formatLocale;
  // Arabic and Hebrew flip the whole layout; the stylesheet is written in
  // logical properties so this one attribute is the entire change.
  document.documentElement.dir = pack.direction ?? 'ltr';

  if (remember) {
    try {
      localStorage.setItem(STORED_LOCALE, code);
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

export function currentFormatLocale() {
  return formatLocale;
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
  const catalogue = await localeCatalogue();
  const packs = catalogue.locales ?? [];
  if (!packs.length) return;

  const resolved = resolveLocale(catalogue);
  await setLocale(resolved.pack, { remember: false, format: resolved.format });

  const stored = readStored();
  const render = () => {
    clear(select);
    // "Automatic" is how someone undoes a choice without clearing storage
    // by hand.
    select.append(el('option', { value: AUTOMATIC, text: t('language.auto') }));
    for (const pack of packs) {
      select.append(el('option', {
        value: pack.locale,
        // The language's own name, so you can find yours without reading
        // the one you do not speak.
        text: pack.name,
      }));
    }
    select.value = stored && stored !== AUTOMATIC ? locale : AUTOMATIC;
  };
  render();
  whenLocaleChanges(render);

  select.addEventListener('change', async () => {
    if (select.value === AUTOMATIC) {
      try {
        localStorage.removeItem(STORED_LOCALE);
      } catch {
        // Nothing stored means automatic anyway.
      }
      const again = resolveLocale({ ...catalogue, locales: packs });
      await setLocale(again.pack, { remember: false, format: again.format });
    } else {
      await setLocale(select.value);
    }
  });
}

// ------------------------------------------------------------ formatting

/**
 * Binary sizes. The unit symbols are international; the number is formatted
 * for the current language, so Arabic gets Arabic-Indic digits and German
 * gets a comma.
 */
export function bytes(n) {
  if (!n) return '0 B';
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
      : new Intl.NumberFormat(formatLocale, {
          maximumFractionDigits: 1,
          minimumFractionDigits: 1,
        }).format(value);
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
  return new Intl.NumberFormat(formatLocale, {
    style: 'percent',
    maximumFractionDigits: 0,
  }).format(fraction || 0);
}

export function number(n) {
  return numbers.format(n ?? 0);
}

export function decimal(n, digits = 2) {
  return new Intl.NumberFormat(formatLocale, {
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

/** Shorten a content id or peer id for display, keeping both ends. */
export function shortId(id, head = 8, tail = 6) {
  if (!id || id.length <= head + tail + 1) return id || '';
  return `${id.slice(0, head)}…${id.slice(-tail)}`;
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

/**
 * Fill in the footer's source link from what the node reports about itself.
 *
 * The URL comes from the build rather than from this file, so a fork that
 * changes `repository` in its `Cargo.toml` — which the AGPL requires it to do
 * once it has modified anything — points at its own source rather than ours.
 * `/v1/about` needs no token: the licence requires the offer to reach anyone
 * who can reach the program.
 */
export async function sourceNotice() {
  const link = document.getElementById('source-link');
  const version = document.getElementById('build-version');
  if (!link && !version) return;
  try {
    const about = await api('/v1/about');
    if (link && about.sourceUrl) link.href = about.sourceUrl;
    if (version) {
      version.textContent = t('footer.build', {
        version: about.version,
        licence: about.licence,
      });
    }
  } catch {
    // The page still works without it; hide the half-filled line rather than
    // show a link that goes nowhere.
    if (link) link.removeAttribute('href');
  }
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
  window.addEventListener('hashchange', () => {
    show();
    // The page changed under a keyboard user who cannot see that it did.
    // Moving focus into the new page puts them at its top, where a sighted
    // user already is. Only on navigation: doing it on first load would steal
    // focus from whatever the browser restored.
    const main = document.getElementById('main');
    if (main) main.focus({ preventScroll: true });
  });
  show();
  return show;
}

export function go(page, arg) {
  location.hash = arg ? `#/${page}/${encodeURIComponent(arg)}` : `#/${page}`;
}
