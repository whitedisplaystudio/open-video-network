// The admin UI: what this node is doing, and the levers for changing it.

import {
  get, post, del, api, bytes, duration, ago, date, decimal, percent, number,
  el, mount, empty, toast, reportError, liveEvents, poll, router, shortId,
  t, languagePicker, whenLocaleChanges, sourceNotice,
} from '/assets/common.js';

const $ = (id) => document.getElementById(id);

/** Download progress, keyed by content id, fed by the event stream. */
const transfers = new Map();

/** How a peer was found, in words rather than protocol names. */
const SOURCE_KEYS = {
  mdns: 'admin.source.mdns',
  dht: 'admin.source.dht',
  url: 'admin.source.url',
  manual: 'admin.source.manual',
  bootstrap: 'admin.source.bootstrap',
  connected: 'admin.source.connected',
};

// --------------------------------------------------------------- overview

function stat(key, value, sub) {
  return el('div', { class: 'stat' }, [
    el('div', { class: 'k', text: key }),
    el('div', { class: 'v', text: value }),
    sub ? el('div', { class: 's', text: sub }) : null,
  ]);
}

/**
 * How to describe whether others can reach this node.
 *
 * The thing a person most needs to know: it decides whether they can share
 * anything, or only watch.
 */
function reachability(status) {
  if (status.reachability === 'public') {
    return [t('admin.reach.direct'), t('admin.reach.direct.sub')];
  }
  if (status.relays > 0) {
    return [
      t('admin.reach.relayed'),
      t('admin.reach.relayed.sub', { count: status.relays }),
    ];
  }
  if (status.reachability === 'private') {
    return [t('admin.reach.blocked'), t('admin.reach.blocked.sub')];
  }
  return [t('admin.reach.unknown'), t('admin.reach.unknown.sub')];
}

async function loadOverview() {
  const status = await get('/v1/status');
  const [reach, reachDetail] = reachability(status);

  mount($('stats'), [
    stat(t('admin.stat.reach'), reach, reachDetail),
    stat(t('admin.stat.peers'), number(status.connectedPeers),
      t('admin.stat.peers.sub', { count: status.knownPeers })),
    stat(t('admin.stat.videos'), number(status.knownVideos),
      t('admin.stat.videos.sub', { count: status.localVideos })),
    stat(t('admin.stat.serving'), number(status.providing), t('admin.stat.serving.sub')),
    stat(t('admin.stat.cache'), bytes(status.cache.totalBytes),
      t('admin.stat.cache.sub', { limit: bytes(status.cacheLimitBytes) })),
    stat(t('admin.stat.uptime'), duration(status.uptimeSecs), status.nodeName),
  ]);

  mount($('identity'), [
    el('dt', { text: t('admin.identity.peerId') }), el('dd', { class: 'mono', text: status.peerId }),
    el('dt', { text: t('admin.identity.publicKey') }), el('dd', { class: 'mono', text: status.publicKey }),
    el('dt', { text: t('admin.identity.dataDir') }), el('dd', { class: 'mono', text: status.dataDir }),
    el('dt', { text: t('admin.identity.listening') }),
    el('dd', {}, status.listenAddrs.length
      ? status.listenAddrs.map((address) => el('div', { class: 'mono', text: address }))
      : [el('span', { text: t('admin.identity.nothingYet') })]),
  ]);

  const used = status.cacheLimitBytes
    ? Math.min(100, (status.cache.totalBytes / status.cacheLimitBytes) * 100)
    : 0;
  $('cache-meter').style.setProperty('--value', used.toFixed(1));
  mount($('storage'), [
    el('dt', { text: t('admin.storage.held') }),
    el('dd', { text: t('admin.storage.heldValue', { bytes: bytes(status.cache.totalBytes), count: status.cache.blockCount }) }),
    el('dt', { text: t('admin.storage.pinned') }),
    el('dd', { text: t('admin.storage.pinnedValue', { bytes: bytes(status.cache.pinnedBytes) }) }),
    el('dt', { text: t('admin.storage.limit') }),
    el('dd', { text: t('admin.storage.limitValue', { bytes: bytes(status.cacheLimitBytes) }) }),
  ]);

  return status;
}

async function loadShareLink() {
  const result = await get('/v1/share-link');
  $('share-link').value = result.shareLink;
}

function renderTransfers() {
  const rows = [...transfers.values()].map((transfer) => {
    const fraction = transfer.total ? transfer.done / transfer.total : 0;
    const meter = el('div', { class: 'meter' }, [el('i')]);
    meter.style.setProperty('--value', String(Math.round(fraction * 100)));
    return el('div', { class: 'transfer' }, [
      el('div', { class: 'who' }, [
        el('div', { class: 'n', text: shortId(transfer.cid, 14, 8) }),
        meter,
      ]),
      el('div', {
        class: 'pct',
        text: transfer.failed ? t('admin.transfers.failed') : percent(fraction),
      }),
      el('div', { class: 'badge', text: bytes(transfer.bytes) }),
    ]);
  });
  mount($('transfers'), rows.length ? rows : empty('↓', 'admin.transfers.empty'));
}

function logLine(text) {
  const log = $('log');
  const stamp = new Date().toLocaleTimeString();
  log.prepend(el('div', {}, [el('span', { class: 't', text: stamp }), text]));
  while (log.childElementCount > 200) log.lastElementChild.remove();
}

// ------------------------------------------------------------------ peers

async function loadPeers() {
  const peers = await get('/v1/peers');
  $('peer-count').textContent = t('admin.peers.known', { count: peers.length });

  if (!peers.length) {
    mount($('peers'), empty('◇', 'admin.peers.empty.title', 'admin.peers.empty.hint'));
    return;
  }

  const rows = peers.map((peer) =>
    el('tr', {}, [
      el('td', { class: 'mono', text: shortId(peer.peerId, 10, 6) }),
      el('td', { text: peer.nodeName || '—' }),
      el('td', {}, [el('span', { class: 'badge', text: t(SOURCE_KEYS[peer.source] ?? 'admin.source.dht') })]),
      el('td', { text: ago(peer.lastSeen) }),
      el('td', { text: peer.lastConnected ? ago(peer.lastConnected) : t('admin.peers.never') }),
      el('td', { class: 'shrink' }, [
        el('button', {
          class: 'small danger',
          type: 'button',
          text: t('admin.peers.forget'),
          onClick: async () => {
            try {
              await del(`/v1/peers/${peer.peerId}`);
              toast(t('admin.peers.forgotten'));
              await loadPeers();
            } catch (error) {
              reportError(error);
            }
          },
        }),
      ]),
    ]),
  );

  const headings = [
    'admin.peers.col.peer', 'admin.peers.col.name', 'admin.peers.col.source',
    'admin.peers.col.lastSeen', 'admin.peers.col.lastConnected', null,
  ];
  mount($('peers'), el('table', {}, [
    el('thead', {}, [el('tr', {}, headings.map((key) => el('th', { text: key ? t(key) : '' })))]),
    el('tbody', {}, rows),
  ]));
}

// ---------------------------------------------------------------- content

async function loadLocalVideos() {
  const videos = await get('/v1/videos/local');
  if (!videos.length) {
    mount($('local-videos'), empty('▲', 'admin.local.empty.title', 'admin.local.empty.hint'));
    return;
  }
  const rows = videos.map((video) =>
    el('tr', {}, [
      el('td', {}, [
        el('div', { text: video.title }),
        el('div', { class: 'mono', text: shortId(video.cid, 16, 8) }),
      ]),
      el('td', { text: video.durationSecs ? duration(video.durationSecs) : '—' }),
      el('td', {}, video.tags.map((tag) => el('span', { class: 'tag', text: tag }))),
      el('td', { text: date(video.createdAt) }),
      el('td', { class: 'shrink' }, [
        el('a', { class: 'badge', href: `/ui#/watch/${video.cid}`, text: t('admin.local.watch') }),
      ]),
    ]),
  );
  const headings = [
    'admin.local.col.video', 'admin.local.col.length', 'admin.local.col.tags',
    'admin.local.col.published', null,
  ];
  mount($('local-videos'), el('table', {}, [
    el('thead', {}, [el('tr', {}, headings.map((key) => el('th', { text: key ? t(key) : '' })))]),
    el('tbody', {}, rows),
  ]));
}

function wireUpload() {
  $('upload-go').addEventListener('click', async () => {
    const input = $('upload-file');
    const file = input.files?.[0];
    if (!file) {
      toast(t('admin.publish.noFile'), 'error');
      return;
    }

    const params = new URLSearchParams({
      fileName: file.name,
      title: $('upload-title').value,
      description: $('upload-description').value,
      tags: $('upload-tags').value,
    });

    const button = $('upload-go');
    button.disabled = true;
    $('upload-state').textContent = t('admin.publish.uploading');
    $('upload-meter').style.setProperty('--value', '5');

    try {
      // The body is streamed to disk on the node, then chunked into the
      // block store, so a large file never has to fit in memory anywhere.
      const result = await api(`/v1/upload?${params}`, {
        method: 'POST',
        body: file,
        headers: { 'Content-Type': 'application/octet-stream' },
      });
      $('upload-meter').style.setProperty('--value', '100');
      $('upload-state').textContent = t('admin.publish.published');
      toast(t(result.announcedToNetwork ? 'admin.publish.done' : 'admin.publish.doneNoPeers',
        { title: result.title }));
      input.value = '';
      $('upload-title').value = '';
      $('upload-description').value = '';
      $('upload-tags').value = '';
      await Promise.all([loadLocalVideos(), loadOverview()]);
    } catch (error) {
      $('upload-state').textContent = t('admin.publish.failed');
      $('upload-meter').style.setProperty('--value', '0');
      reportError(error);
    } finally {
      button.disabled = false;
    }
  });
}

function wireProfile() {
  $('profile-save').addEventListener('click', async () => {
    const displayName = $('profile-name').value.trim();
    if (!displayName) {
      toast(t('admin.profile.needName'), 'error');
      return;
    }
    try {
      await post('/v1/profile', { displayName, bio: $('profile-bio').value });
      toast(t('admin.profile.saved'));
    } catch (error) {
      reportError(error);
    }
  });
}

// ------------------------------------------------------------- moderation

async function loadModeration() {
  const [cids, creators] = await Promise.all([get('/v1/blocked/cids'), get('/v1/blocked/creators')]);

  const list = (entries, onRemove) =>
    entries.length
      ? el('table', {}, [
          el('tbody', {}, entries.map((entry) =>
            el('tr', {}, [
              el('td', {}, [
                el('div', { class: 'mono', text: shortId(entry.subject, 14, 8) }),
                entry.reason ? el('div', { class: 'meta', text: entry.reason }) : null,
              ]),
              el('td', { class: 'shrink' }, [
                el('button', {
                  class: 'small', type: 'button', text: t('admin.moderation.unblock'),
                  onClick: () => onRemove(entry.subject),
                }),
              ]),
            ]),
          )),
        ])
      : empty('○', 'admin.moderation.empty');

  mount($('blocked-cids'), list(cids, async (cid) => {
    try {
      await del(`/v1/blocked/cids/${cid}`);
      await loadModeration();
    } catch (error) {
      reportError(error);
    }
  }));

  mount($('blocked-creators'), list(creators, async (key) => {
    try {
      await del(`/v1/blocked/creators/${key}`);
      await loadModeration();
    } catch (error) {
      reportError(error);
    }
  }));
}

function wireModeration() {
  const block = async (input, path) => {
    const subject = input.value.trim();
    if (!subject) return;
    try {
      await post(`${path}/${encodeURIComponent(subject)}`, { reason: t('admin.moderation.reason') });
      input.value = '';
      toast(t('admin.moderation.blocked'));
      await loadModeration();
    } catch (error) {
      reportError(error);
    }
  };
  $('block-cid-go').addEventListener('click', () => block($('block-cid'), '/v1/blocked/cids'));
  $('block-creator-go').addEventListener('click', () => block($('block-creator'), '/v1/blocked/creators'));
}

// ---------------------------------------------------------------- privacy

async function loadPrivacy() {
  const [preferences, history] = await Promise.all([get('/v1/preferences'), get('/v1/watch?limit=40')]);

  mount($('preferences'), preferences.length
    ? preferences.map((pref) => {
        const meter = el('div', { class: 'meter' }, [el('i')]);
        meter.style.setProperty('--value', String(Math.abs(pref.weight) * 100));
        return el('div', { class: 'transfer' }, [
          el('div', { class: 'who' }, [el('div', { class: 'n', text: pref.tag }), meter]),
          el('div', { class: 'pct', text: decimal(pref.weight, 2) }),
        ]);
      })
    : empty('◌', 'admin.privacy.model.empty.title', 'admin.privacy.model.empty.hint'));

  const summary = history.summary;
  mount($('watch-summary'), el('p', {
    class: 'lede',
    text: t('admin.privacy.summary', {
      events: number(summary.eventCount),
      videos: number(summary.distinctVideos),
      duration: duration(summary.totalWatchedSecs),
    }),
  }));

  const headings = [
    'admin.privacy.col.video', 'admin.privacy.col.watched',
    'admin.privacy.col.of', 'admin.privacy.col.when',
  ];
  mount($('watch-history'), history.entries.length
    ? el('table', {}, [
        el('thead', {}, [el('tr', {}, headings.map((key) => el('th', { text: t(key) })))]),
        el('tbody', {}, history.entries.map((entry) =>
          el('tr', {}, [
            el('td', { class: 'mono', text: shortId(entry.cid, 10, 6) }),
            el('td', { text: percent(entry.ratio) }),
            el('td', { text: duration(entry.durationSecs) }),
            el('td', { text: ago(entry.watchedAt) }),
          ]),
        )),
      ])
    : empty('○', 'admin.privacy.history.empty'));
}

function wirePrivacy() {
  $('clear-history').addEventListener('click', async () => {
    if (!confirm(t('admin.privacy.erase.confirm'))) return;
    try {
      await del('/v1/watch');
      toast(t('admin.privacy.erase.done'));
      await loadPrivacy();
    } catch (error) {
      reportError(error);
    }
  });
}

// ------------------------------------------------------------------- boot

function wireShareLink() {
  $('copy-link').addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText($('share-link').value);
      $('copy-state').textContent = t('admin.invite.copied');
    } catch {
      // Clipboard access can be refused; selecting it is still useful.
      $('share-link').select();
      $('copy-state').textContent = t('admin.invite.copyManually');
    }
    setTimeout(() => { $('copy-state').textContent = ''; }, 2500);
  });
}

function wirePeerAdd() {
  $('peer-add').addEventListener('click', async () => {
    const target = $('peer-target').value.trim();
    if (!target) return;
    const button = $('peer-add');
    button.disabled = true;
    try {
      const result = await post('/v1/peers', { target });
      toast(t('admin.peers.connected', { name: result.nodeName || shortId(result.peerId) }));
      $('peer-target').value = '';
      await loadPeers();
    } catch (error) {
      reportError(error);
    } finally {
      button.disabled = false;
    }
  });
}

function wireShutdown() {
  $('shutdown').addEventListener('click', async () => {
    if (!confirm(t('admin.shutdown.confirm'))) return;
    try {
      await post('/v1/shutdown', {});
      toast(t('admin.shutdown.toast'));
    } catch (error) {
      reportError(error);
    }
  });
}

function setConnection(label, kind) {
  $('connection-text').textContent = label;
  $('connection').className = `badge ${kind}`;
}

function wireEvents() {
  liveEvents({
    connected: () => setConnection(t('conn.live'), 'ok'),
    disconnected: () => setConnection(t('conn.offline'), 'danger'),
    peerConnected: (e) => {
      logLine(t('admin.log.peerConnected', { peer: shortId(e.peerId) }));
      loadPeers().catch(() => {});
    },
    peerDisconnected: (e) => logLine(t('admin.log.peerDisconnected', { peer: shortId(e.peerId) })),
    videoDiscovered: (e) => logLine(t('admin.log.discovered', { title: e.title })),
    publishStarted: (e) => logLine(t('admin.log.publishing', { file: e.fileName })),
    publishCompleted: (e) => logLine(t('admin.log.published', { title: e.title })),
    fetchStarted: (e) => {
      transfers.set(e.cid, { cid: e.cid, done: e.alreadyHeld, total: e.totalChunks, bytes: 0 });
      renderTransfers();
      logLine(t('admin.log.fetching', { cid: shortId(e.cid), count: e.totalChunks }));
    },
    fetchProgress: (e) => {
      transfers.set(e.cid, {
        cid: e.cid, done: e.completedChunks, total: e.totalChunks, bytes: e.bytesFetched,
      });
      renderTransfers();
    },
    fetchCompleted: (e) => {
      logLine(t('admin.log.fetched', { cid: shortId(e.cid), bytes: bytes(e.bytesFetched) }));
      transfers.delete(e.cid);
      renderTransfers();
      loadOverview().catch(() => {});
    },
    fetchFailed: (e) => {
      const existing = transfers.get(e.cid) ?? { cid: e.cid, done: 0, total: 1, bytes: 0 };
      transfers.set(e.cid, { ...existing, failed: true });
      renderTransfers();
      logLine(t('admin.log.fetchFailed', { cid: shortId(e.cid), error: e.error }));
    },
  });
}

const LOADERS = {
  overview: () => Promise.all([loadOverview(), loadShareLink()]),
  peers: loadPeers,
  content: () => Promise.all([loadOverview(), loadLocalVideos()]),
  moderation: loadModeration,
  privacy: loadPrivacy,
};

function currentPage() {
  return location.hash.replace(/^#\/?/, '').split('/')[0] || 'overview';
}

// Your own channel link, so it can be handed to somebody.
async function loadChannelLink() {
  const field = $('channel-link');
  if (!field) return;
  try {
    const { link } = await get('/v1/channel/link');
    field.value = link;
  } catch {
    field.value = '';
  }
  $('copy-channel-link')?.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(field.value);
      toast(t('admin.channel.copied'));
    } catch {
      // Clipboard access can be refused; selecting it is the fallback that
      // always works.
      field.select();
    }
  });
}

async function boot() {
  // Language first, so nothing renders in English and then flips.
  await languagePicker($('language'));

  wireShareLink();
  wirePeerAdd();
  wireUpload();
  wireProfile();
  wireModeration();
  wirePrivacy();
  wireShutdown();
  renderTransfers();

  router((page) => LOADERS[page]?.().catch(reportError));

  whenLocaleChanges(() => {
    renderTransfers();
    LOADERS[currentPage()]?.().catch(reportError);
  });

  try {
    await loadOverview();
    await loadShareLink();
  } catch (error) {
    reportError(error);
  }

  loadChannelLink();
  sourceNotice();
  wireEvents();
  poll(() => loadOverview(), 5000);
}

boot();
