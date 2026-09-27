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
    throw new ApiError(
      'This page is not authorised. Run `ourvideo ui` and open the link it prints.',
      401,
    );
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

// ------------------------------------------------------------ formatting

export function bytes(n) {
  if (!n) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${n} B` : `${value.toFixed(1)} ${units[unit]}`;
}

export function duration(secs) {
  secs = Math.max(0, Math.round(secs || 0));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  const pad = (n) => String(n).padStart(2, '0');
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}

export function ago(unixSeconds) {
  if (!unixSeconds) return 'never';
  const delta = Math.max(0, Math.floor(Date.now() / 1000 - unixSeconds));
  if (delta < 60) return 'just now';
  const steps = [
    [60, 'minute'],
    [24, 'hour'],
    [7, 'day'],
    [4.35, 'week'],
    [12, 'month'],
  ];
  let value = delta / 60;
  let unit = 'minute';
  for (const [factor, next] of steps) {
    if (value < factor) break;
    value /= factor;
    unit = next;
  }
  const rounded = Math.floor(value);
  return `${rounded} ${unit}${rounded === 1 ? '' : 's'} ago`;
}

export function shortId(id, head = 8, tail = 6) {
  if (!id || id.length <= head + tail + 1) return id || '';
  return `${id.slice(0, head)}…${id.slice(-tail)}`;
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

export function empty(icon, title, hint) {
  return el('div', { class: 'empty' }, [
    el('div', { class: 'big', text: icon }),
    el('div', { text: title }),
    hint ? el('p', { class: 'lede', text: hint }) : null,
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
