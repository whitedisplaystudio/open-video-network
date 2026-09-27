// The admin UI: what this node is doing, and the levers for changing it.

import {
  get, post, del, api, bytes, duration, ago, el, mount, clear, empty,
  toast, reportError, liveEvents, poll, router, shortId,
} from '/assets/common.js';

const $ = (id) => document.getElementById(id);

/** Download progress, keyed by content id, fed by the event stream. */
const transfers = new Map();

// --------------------------------------------------------------- overview

function stat(key, value, sub) {
  return el('div', { class: 'stat' }, [
    el('div', { class: 'k', text: key }),
    el('div', { class: 'v', text: value }),
    sub ? el('div', { class: 's', text: sub }) : null,
  ]);
}

async function loadOverview() {
  const status = await get('/v1/status');

  mount($('stats'), [
    stat('Peers', String(status.connectedPeers), `${status.knownPeers} known`),
    stat('Videos', String(status.knownVideos), `${status.localVideos} published here`),
    stat('Serving', String(status.providing), 'announced to the DHT'),
    stat('Cache', bytes(status.cache.totalBytes), `of ${bytes(status.cacheLimitBytes)}`),
    stat('Uptime', duration(status.uptimeSecs), status.nodeName),
  ]);

  mount($('identity'), [
    el('dt', { text: 'Peer id' }), el('dd', { class: 'mono', text: status.peerId }),
    el('dt', { text: 'Public key' }), el('dd', { class: 'mono', text: status.publicKey }),
    el('dt', { text: 'Data directory' }), el('dd', { class: 'mono', text: status.dataDir }),
    el('dt', { text: 'Listening on' }),
    el('dd', {}, status.listenAddrs.length
      ? status.listenAddrs.map((a) => el('div', { class: 'mono', text: a }))
      : [el('span', { text: 'nothing yet' })]),
  ]);

  const used = status.cacheLimitBytes
    ? Math.min(100, (status.cache.totalBytes / status.cacheLimitBytes) * 100)
    : 0;
  $('cache-meter').style.setProperty('--value', used.toFixed(1));
  mount($('storage'), [
    el('dt', { text: 'Held' }), el('dd', { text: `${bytes(status.cache.totalBytes)} in ${status.cache.blockCount} blocks` }),
    el('dt', { text: 'Pinned' }), el('dd', { text: `${bytes(status.cache.pinnedBytes)} — published here, never evicted` }),
    el('dt', { text: 'Limit' }), el('dd', { text: `${bytes(status.cacheLimitBytes)} of fetched content` }),
  ]);

  return status;
}

async function loadShareLink() {
  const result = await get('/v1/share-link');
  $('share-link').value = result.shareLink;
}

function renderTransfers() {
  const rows = [...transfers.values()].map((t) => {
    const pct = t.total ? Math.round((t.done / t.total) * 100) : 0;
    const meter = el('div', { class: 'meter' }, [el('i')]);
    meter.style.setProperty('--value', String(pct));
    return el('div', { class: 'transfer' }, [
      el('div', { class: 'who' }, [
        el('div', { class: 'n', text: shortId(t.cid, 14, 8) }),
        meter,
      ]),
      el('div', { class: 'pct', text: t.failed ? 'failed' : `${pct}%` }),
      el('div', { class: 'badge', text: bytes(t.bytes) }),
    ]);
  });
  mount($('transfers'), rows.length ? rows : empty('↓', 'No transfers in progress'));
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
  $('peer-count').textContent = `${peers.length} known`;

  if (!peers.length) {
    mount($('peers'), empty('◇', 'No peers yet', 'Paste a link above, or start another node on this network.'));
    return;
  }

  const rows = peers.map((peer) =>
    el('tr', {}, [
      el('td', { class: 'mono', text: shortId(peer.peerId, 10, 6) }),
      el('td', { text: peer.nodeName || '—' }),
      el('td', {}, [el('span', { class: 'badge', text: peer.source })]),
      el('td', { text: ago(peer.lastSeen) }),
      el('td', { text: peer.lastConnected ? ago(peer.lastConnected) : 'never' }),
      el('td', { class: 'shrink' }, [
        el('button', {
          class: 'small danger',
          type: 'button',
          text: 'Forget',
          onClick: async () => {
            try {
              await del(`/v1/peers/${peer.peerId}`);
              toast('Forgotten.');
              await loadPeers();
            } catch (error) {
              reportError(error);
            }
          },
        }),
      ]),
    ]),
  );

  mount($('peers'), el('table', {}, [
    el('thead', {}, [
      el('tr', {}, ['Peer', 'Name', 'Found via', 'Last seen', 'Last connected', ''].map((h) => el('th', { text: h }))),
    ]),
    el('tbody', {}, rows),
  ]));
}

// ---------------------------------------------------------------- content

async function loadLocalVideos() {
  const videos = await get('/v1/videos/local');
  if (!videos.length) {
    mount($('local-videos'), empty('▲', 'Nothing published here yet', 'Pick a file above.'));
    return;
  }
  const rows = videos.map((video) =>
    el('tr', {}, [
      el('td', {}, [
        el('div', { text: video.title }),
        el('div', { class: 'mono', text: shortId(video.cid, 16, 8) }),
      ]),
      el('td', { text: video.durationSecs ? duration(video.durationSecs) : '—' }),
      el('td', {}, video.tags.map((t) => el('span', { class: 'tag', text: t }))),
      el('td', { text: new Date(video.createdAt * 1000).toLocaleDateString() }),
      el('td', { class: 'shrink' }, [
        el('a', { class: 'badge', href: `/ui#/watch/${video.cid}`, text: 'Watch' }),
      ]),
    ]),
  );
  mount($('local-videos'), el('table', {}, [
    el('thead', {}, [el('tr', {}, ['Video', 'Length', 'Tags', 'Published', ''].map((h) => el('th', { text: h })))]),
    el('tbody', {}, rows),
  ]));
}

function wireUpload() {
  $('upload-go').addEventListener('click', async () => {
    const input = $('upload-file');
    const file = input.files?.[0];
    if (!file) {
      toast('Choose a file first.', 'error');
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
    $('upload-state').textContent = 'uploading…';
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
      $('upload-state').textContent = 'published';
      toast(
        result.announcedToNetwork
          ? `Published "${result.title}" and announced it.`
          : `Published "${result.title}". No peers are listening yet, so nobody has been told.`,
      );
      input.value = '';
      $('upload-title').value = '';
      $('upload-description').value = '';
      $('upload-tags').value = '';
      await Promise.all([loadLocalVideos(), loadOverview()]);
    } catch (error) {
      $('upload-state').textContent = 'failed';
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
      toast('A display name is required.', 'error');
      return;
    }
    try {
      await post('/v1/profile', { displayName, bio: $('profile-bio').value });
      toast('Profile signed and announced.');
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
                  class: 'small', type: 'button', text: 'Unblock',
                  onClick: () => onRemove(entry.subject),
                }),
              ]),
            ]),
          )),
        ])
      : empty('○', 'Nothing blocked');

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
  $('block-cid-go').addEventListener('click', async () => {
    const cid = $('block-cid').value.trim();
    if (!cid) return;
    try {
      await post(`/v1/blocked/cids/${encodeURIComponent(cid)}`, { reason: 'blocked from the admin page' });
      $('block-cid').value = '';
      toast('Blocked here. No other node is affected.');
      await loadModeration();
    } catch (error) {
      reportError(error);
    }
  });

  $('block-creator-go').addEventListener('click', async () => {
    const key = $('block-creator').value.trim();
    if (!key) return;
    try {
      await post(`/v1/blocked/creators/${encodeURIComponent(key)}`, { reason: 'blocked from the admin page' });
      $('block-creator').value = '';
      toast('Blocked here. No other node is affected.');
      await loadModeration();
    } catch (error) {
      reportError(error);
    }
  });
}

// ---------------------------------------------------------------- privacy

async function loadPrivacy() {
  const [preferences, history] = await Promise.all([get('/v1/preferences'), get('/v1/watch?limit=40')]);

  mount(
    $('preferences'),
    preferences.length
      ? preferences.map((pref) => {
          const meter = el('div', { class: 'meter' }, [el('i')]);
          meter.style.setProperty('--value', String(Math.abs(pref.weight) * 100));
          return el('div', { class: 'transfer' }, [
            el('div', { class: 'who' }, [el('div', { class: 'n', text: pref.tag }), meter]),
            el('div', { class: 'pct', text: pref.weight.toFixed(2) }),
          ]);
        })
      : empty('◌', 'Nothing learned yet', 'Watch a few videos and a tag model appears here.'),
  );

  const summary = history.summary;
  mount($('watch-summary'), el('p', {
    class: 'lede',
    text: `${summary.eventCount} viewing events across ${summary.distinctVideos} videos, ${duration(summary.totalWatchedSecs)} in total.`,
  }));

  mount(
    $('watch-history'),
    history.entries.length
      ? el('table', {}, [
          el('thead', {}, [el('tr', {}, ['Video', 'Watched', 'Of', 'When'].map((h) => el('th', { text: h })))]),
          el('tbody', {}, history.entries.map((entry) =>
            el('tr', {}, [
              el('td', { class: 'mono', text: shortId(entry.cid, 10, 6) }),
              el('td', { text: `${Math.round(entry.ratio * 100)}%` }),
              el('td', { text: duration(entry.durationSecs) }),
              el('td', { text: ago(entry.watchedAt) }),
            ]),
          )),
        ])
      : empty('○', 'No viewing recorded'),
  );
}

function wirePrivacy() {
  $('clear-history').addEventListener('click', async () => {
    if (!confirm('Erase all viewing history and the preference model on this node?')) return;
    try {
      await del('/v1/watch');
      toast('Erased.');
      await loadPrivacy();
    } catch (error) {
      reportError(error);
    }
  });
}

// ------------------------------------------------------------------- boot

function wireShareLink() {
  $('copy-link').addEventListener('click', async () => {
    const value = $('share-link').value;
    try {
      await navigator.clipboard.writeText(value);
      $('copy-state').textContent = 'copied';
    } catch {
      // Clipboard access can be refused; selecting it is still useful.
      $('share-link').select();
      $('copy-state').textContent = 'press ⌘C';
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
      toast(`Connected to ${result.nodeName || shortId(result.peerId)}.`);
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
    if (!confirm('Shut this node down? The network carries on without it.')) return;
    try {
      await post('/v1/shutdown', {});
      toast('Shutting down.');
    } catch (error) {
      reportError(error);
    }
  });
}

function wireEvents() {
  const badge = $('connection');
  const text = $('connection-text');
  const set = (label, kind) => {
    text.textContent = label;
    badge.className = `badge ${kind}`;
  };

  liveEvents({
    connected: () => set('live', 'ok'),
    disconnected: () => set('offline', 'danger'),
    peerConnected: (e) => { logLine(`peer connected ${shortId(e.peerId)}`); loadPeers().catch(() => {}); },
    peerDisconnected: (e) => { logLine(`peer disconnected ${shortId(e.peerId)}`); },
    videoDiscovered: (e) => logLine(`discovered "${e.title}"`),
    publishStarted: (e) => logLine(`publishing ${e.fileName}`),
    publishCompleted: (e) => logLine(`published "${e.title}"`),
    fetchStarted: (e) => {
      transfers.set(e.cid, { cid: e.cid, done: e.alreadyHeld, total: e.totalChunks, bytes: 0 });
      renderTransfers();
      logLine(`fetching ${shortId(e.cid)} — ${e.totalChunks} chunks`);
    },
    fetchProgress: (e) => {
      transfers.set(e.cid, { cid: e.cid, done: e.completedChunks, total: e.totalChunks, bytes: e.bytesFetched });
      renderTransfers();
    },
    fetchCompleted: (e) => {
      logLine(`fetched ${shortId(e.cid)} — ${bytes(e.bytesFetched)}`);
      transfers.delete(e.cid);
      renderTransfers();
      loadOverview().catch(() => {});
    },
    fetchFailed: (e) => {
      const existing = transfers.get(e.cid) || { cid: e.cid, done: 0, total: 1, bytes: 0 };
      transfers.set(e.cid, { ...existing, failed: true });
      renderTransfers();
      logLine(`fetch failed ${shortId(e.cid)}: ${e.error}`);
    },
  });
}

async function boot() {
  wireShareLink();
  wirePeerAdd();
  wireUpload();
  wireProfile();
  wireModeration();
  wirePrivacy();
  wireShutdown();
  renderTransfers();

  router((page) => {
    const load = {
      overview: () => Promise.all([loadOverview(), loadShareLink()]),
      peers: loadPeers,
      content: () => Promise.all([loadOverview(), loadLocalVideos()]),
      moderation: loadModeration,
      privacy: loadPrivacy,
    }[page];
    load?.().catch(reportError);
  });

  try {
    await loadOverview();
    await loadShareLink();
  } catch (error) {
    reportError(error);
  }

  wireEvents();
  poll(() => loadOverview(), 5000);
}

boot();
