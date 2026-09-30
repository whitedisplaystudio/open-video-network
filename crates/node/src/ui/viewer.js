// The viewer UI: browse, search, watch, and a feed computed on this device.

import {
  get, post, bytes, duration, date, decimal, el, mount, empty, toast, reportError,
  liveEvents, poll, router, go, shortId, t, languagePicker, whenLocaleChanges,
  sourceNotice,
} from '/assets/common.js';

const $ = (id) => document.getElementById(id);

const state = {
  videos: [],
  byCid: new Map(),
};

// ------------------------------------------------------------ video cards

function videoCard(video) {
  const thumb = el('div', { class: 'thumb' }, [
    el('img', {
      src: `/v1/videos/${video.cid}/thumbnail`,
      alt: '',
      loading: 'lazy',
      // Most videos have no thumbnail until FFmpeg is around; fall back
      // quietly rather than showing a broken image.
      onError: (event) => {
        event.target.replaceWith(el('span', { class: 'placeholder', text: '▶' }));
      },
    }),
    video.durationSecs ? el('span', { class: 'duration', text: duration(video.durationSecs) }) : null,
  ]);

  return el(
    'button',
    { class: 'video-card', type: 'button', onClick: () => go('watch', video.cid) },
    [
      thumb,
      el('div', { class: 'body' }, [
        el('div', { class: 'title', text: video.title }),
        el('div', { class: 'meta', text: metaLine(video) }),
        video.tags.length
          ? el('div', { class: 'tags' }, video.tags.slice(0, 3).map((tag) => el('span', { class: 'tag', text: tag })))
          : null,
      ]),
    ],
  );
}

function metaLine(video) {
  const where = video.isLocal
    ? t('viewer.card.publishedHere')
    : video.haveContent
      ? t('viewer.card.heldLocally')
      : t('viewer.card.onTheNetwork');
  return `${where} · ${shortId(video.creator, 6, 4)}`;
}

function renderGrid(node, videos, emptyNode) {
  mount(node, videos.length ? videos.map(videoCard) : emptyNode);
}

// -------------------------------------------------------------- data load

async function loadVideos() {
  state.videos = await get('/v1/videos?limit=200');
  state.byCid = new Map(state.videos.map((v) => [v.cid, v]));

  renderGrid($('recent'), state.videos.slice(0, 12),
    empty('◌', 'viewer.empty.recent.title', 'viewer.empty.recent.hint'));
  renderGrid($('browse'), state.videos,
    empty('◌', 'viewer.empty.browse.title', 'viewer.empty.browse.hint'));
  renderGrid($('library'), state.videos.filter((v) => v.haveContent || v.isLocal),
    empty('▤', 'viewer.empty.library.title', 'viewer.empty.library.hint'));
}

async function loadFeed() {
  const feed = await get('/v1/recommendations?limit=12');
  const cards = feed.map((r) => state.byCid.get(r.cid)).filter(Boolean).map(videoCard);
  mount($('feed'), cards.length
    ? cards
    : empty('☆', 'viewer.empty.feed.title', 'viewer.empty.feed.hint'));
  $('recent-heading').hidden = cards.length === 0;
}

// ------------------------------------------------------------------ watch

/**
 * Tracks how much of a video was actually played.
 *
 * Time is accumulated between `timeupdate` events and only when they are
 * close together, so seeking forward does not count as watching. The result
 * is written to this node's database and nowhere else.
 */
class WatchTracker {
  constructor(cid, durationSecs) {
    this.cid = cid;
    this.durationSecs = durationSecs || 0;
    this.watched = 0;
    this.last = null;
    this.liked = false;
    this.completed = false;
    this.reported = 0;
  }

  tick(currentTime) {
    if (this.last !== null) {
      const delta = currentTime - this.last;
      // A jump means a seek, and a negative delta means a rewind.
      if (delta > 0 && delta < 2) this.watched += delta;
    }
    this.last = currentTime;
  }

  seeked(currentTime) {
    this.last = currentTime;
  }

  async report({ skipped = false } = {}) {
    const seconds = Math.round(this.watched);
    if (seconds <= 0 || seconds === this.reported) return;
    this.reported = seconds;
    try {
      await post('/v1/watch', {
        cid: this.cid,
        watchedSecs: seconds,
        durationSecs: Math.round(this.durationSecs),
        completed: this.completed,
        skipped,
        liked: this.liked,
      });
    } catch (error) {
      reportError(error);
    }
  }
}

let tracker = null;
let playerCleanup = null;

async function openWatch(cid) {
  const player = $('player');
  if (playerCleanup) playerCleanup();

  let video = state.byCid.get(cid);
  if (!video) {
    try {
      video = await get(`/v1/videos/${encodeURIComponent(cid)}`);
    } catch (error) {
      reportError(error);
      go('browse');
      return;
    }
  }

  $('watch-title').textContent = video.title;
  $('watch-description').textContent = video.description || '';
  mount($('watch-tags'), video.tags.map((tag) => el('span', { class: 'tag', text: tag })));
  mount($('watch-meta'), [
    el('span', {
      class: 'badge',
      text: video.isLocal ? t('viewer.card.publishedHere') : t('viewer.watch.fromNetwork'),
    }),
    el('span', {
      class: `badge${video.haveContent ? ' ok' : ''}`,
      text: video.haveContent ? t('viewer.card.heldLocally') : t('viewer.watch.streaming'),
    }),
    video.durationSecs ? el('span', { class: 'badge', text: duration(video.durationSecs) }) : null,
  ]);

  mount($('watch-details'), [
    el('dt', { text: t('viewer.details.cid') }), el('dd', { class: 'mono', text: video.cid }),
    el('dt', { text: t('viewer.details.creator') }), el('dd', { class: 'mono', text: video.creator }),
    el('dt', { text: t('viewer.details.announced') }), el('dd', { text: date(video.createdAt) }),
  ]);

  // Streaming: chunks are fetched from peers as the player asks for them.
  player.src = `/v1/videos/${encodeURIComponent(cid)}/stream`;
  player.load();

  tracker = new WatchTracker(cid, video.durationSecs);
  const onTime = () => tracker.tick(player.currentTime);
  const onSeek = () => tracker.seeked(player.currentTime);
  const onEnded = () => {
    tracker.completed = true;
    tracker.report().then(refreshWhy);
  };
  const onPause = () => tracker.report().then(refreshWhy);
  const onMeta = () => {
    if (!tracker.durationSecs && Number.isFinite(player.duration)) {
      tracker.durationSecs = player.duration;
    }
  };
  const onError = () => toast(t('viewer.toast.playFailed'), 'error');

  player.addEventListener('timeupdate', onTime);
  player.addEventListener('seeked', onSeek);
  player.addEventListener('ended', onEnded);
  player.addEventListener('pause', onPause);
  player.addEventListener('loadedmetadata', onMeta);
  player.addEventListener('error', onError);

  playerCleanup = () => {
    for (const [event, handler] of [
      ['timeupdate', onTime], ['seeked', onSeek], ['ended', onEnded],
      ['pause', onPause], ['loadedmetadata', onMeta], ['error', onError],
    ]) {
      player.removeEventListener(event, handler);
    }
    player.pause();
    player.removeAttribute('src');
    player.load();
    const finished = tracker;
    tracker = null;
    finished?.report({ skipped: !finished.completed && finished.watched > 2 });
  };

  wireWatchActions(video);
  refreshWhy();
}

function wireWatchActions(video) {
  const like = $('like');
  like.textContent = t('viewer.watch.like');
  like.classList.remove('primary');
  like.onclick = () => {
    if (!tracker) return;
    tracker.liked = !tracker.liked;
    like.textContent = tracker.liked ? t('viewer.watch.liked') : t('viewer.watch.like');
    like.classList.toggle('primary', tracker.liked);
    tracker.report().then(refreshWhy);
  };

  $('follow').onclick = async () => {
    try {
      await post(`/v1/follow/${video.creator}`);
      toast(t('viewer.toast.following'));
      refreshWhy();
    } catch (error) {
      reportError(error);
    }
  };

  $('download').onclick = async () => {
    try {
      toast(t('viewer.toast.fetching'));
      await post(`/v1/videos/${video.cid}/fetch`, {});
      const result = await post(`/v1/videos/${video.cid}/export`, {});
      toast(t('viewer.toast.saved', { path: result.path }));
      await loadVideos();
    } catch (error) {
      reportError(error);
    }
  };

  $('block').onclick = async () => {
    try {
      await post(`/v1/blocked/cids/${video.cid}`, { reason: t('admin.moderation.reason') });
      toast(t('viewer.toast.hidden'));
      await loadVideos();
      go('browse');
    } catch (error) {
      reportError(error);
    }
  };
}

async function refreshWhy() {
  const cid = location.hash.split('/')[2];
  if (!cid) return;
  try {
    const explanation = await get(`/v1/recommendations/${encodeURIComponent(cid)}`);
    const items = explanation.reasons.map((reason) =>
      el('li', {}, [
        el('span', {
          class: 'n',
          // `tag`, `freshness` and the rest are the engine's own factor
          // names; they are shown as-is, with the tag beside them.
          text: reason.detail ? `${reason.factor} · ${reason.detail}` : reason.factor,
        }),
        el('span', {
          class: `v ${reason.value >= 0 ? 'pos' : 'neg'}`,
          text: decimal(reason.value, 3),
        }),
      ]),
    );
    items.push(el('li', {}, [
      el('span', { class: 'n', text: t('viewer.watch.score') }),
      el('span', { class: 'v', text: decimal(explanation.score, 3) }),
    ]));
    mount($('why'), items);
  } catch {
    mount($('why'), el('li', {}, [el('span', { class: 'n', text: t('viewer.watch.noModel') })]));
  }
}

// ----------------------------------------------------------------- search

async function runSearch(query) {
  $('search-input').value = query;
  if (!query.trim()) {
    mount($('results'), empty('⌕', 'viewer.empty.searchPrompt'));
    $('search-count').textContent = '';
    return;
  }
  const results = await get(`/v1/search?q=${encodeURIComponent(query)}&limit=60`);
  $('search-count').textContent = t('viewer.search.results', { count: results.length });
  renderGrid($('results'), results,
    empty('⌕', 'viewer.empty.search.title', 'viewer.empty.search.hint'));
}

// ------------------------------------------------------------------- boot

let lastPeerCount = 0;

function setConnection(label, kind) {
  $('connection-text').textContent = label;
  $('connection').className = `badge ${kind}`;
}

function watchConnection() {
  liveEvents({
    connected: () => setConnection(t('conn.live'), 'ok'),
    disconnected: () => setConnection(t('conn.offline'), 'danger'),
    videoDiscovered: async (event) => {
      toast(t('viewer.toast.newVideo', { title: event.title }));
      await loadVideos();
    },
    fetchCompleted: () => loadVideos(),
  });

  poll(async () => {
    const status = await get('/v1/status');
    lastPeerCount = status.connectedPeers;
    setConnection(
      t('conn.peers', { count: lastPeerCount }),
      lastPeerCount > 0 ? 'ok' : 'warn',
    );
  }, 5000);
}

function currentPage() {
  return location.hash.replace(/^#\/?/, '').split('/')[0] || 'home';
}

function currentArg() {
  return decodeURIComponent(location.hash.replace(/^#\/?/, '').split('/').slice(1).join('/'));
}

async function renderCurrent() {
  const page = currentPage();
  if (page === 'watch') return openWatch(currentArg());
  if (page === 'search') return runSearch(currentArg());
  if (page === 'home') return loadFeed();
  return undefined;
}

async function boot() {
  // Language first: everything rendered afterwards is already translated.
  await languagePicker($('language'));

  $('search-form').addEventListener('submit', (event) => {
    event.preventDefault();
    go('search', $('search-input').value);
  });

  router((page) => {
    if (page !== 'watch' && playerCleanup) {
      playerCleanup();
      playerCleanup = null;
    }
    renderCurrent().catch(reportError);
  });

  window.addEventListener('pagehide', () => playerCleanup?.());

  // Switching language re-renders whatever is on screen, including the
  // lists, which hold translated labels of their own.
  whenLocaleChanges(() => {
    setConnection(t('conn.peers', { count: lastPeerCount }), lastPeerCount > 0 ? 'ok' : 'warn');
    loadVideos().then(renderCurrent).catch(reportError);
  });

  try {
    await loadVideos();
    await loadFeed();
  } catch (error) {
    reportError(error);
  }
  sourceNotice();
  watchConnection();
}

boot();
