// The viewer UI: browse, search, watch, and a feed computed on this device.

import {
  api, get, post, del, bytes, duration, el, mount, empty, toast, reportError,
  liveEvents, poll, router, go, shortId,
} from '/assets/common.js';

const $ = (id) => document.getElementById(id);

const state = {
  videos: [],
  byCid: new Map(),
  self: null,
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
    {
      class: 'video-card',
      type: 'button',
      onClick: () => go('watch', video.cid),
    },
    [
      thumb,
      el('div', { class: 'body' }, [
        el('div', { class: 'title', text: video.title }),
        el('div', { class: 'meta', text: metaLine(video) }),
        video.tags.length
          ? el('div', { class: 'tags' }, video.tags.slice(0, 3).map((t) => el('span', { class: 'tag', text: t })))
          : null,
      ]),
    ],
  );
}

function metaLine(video) {
  const parts = [];
  if (video.isLocal) parts.push('published here');
  else if (video.haveContent) parts.push('held locally');
  else parts.push('on the network');
  parts.push(shortId(video.creator, 6, 4));
  return parts.join(' · ');
}

function renderGrid(node, videos, emptyNode) {
  mount(node, videos.length ? videos.map(videoCard) : emptyNode);
}

// -------------------------------------------------------------- data load

async function loadVideos() {
  state.videos = await get('/v1/videos?limit=200');
  state.byCid = new Map(state.videos.map((v) => [v.cid, v]));

  renderGrid(
    $('recent'),
    state.videos.slice(0, 12),
    empty('◌', 'Nothing discovered yet', 'Connect to a peer from the admin page, or wait for one on your network.'),
  );
  renderGrid(
    $('browse'),
    state.videos,
    empty('◌', 'Nothing discovered yet', 'A node with no peers hears nothing. Add one and announcements will arrive.'),
  );
  renderGrid(
    $('library'),
    state.videos.filter((v) => v.haveContent || v.isLocal),
    empty('▤', 'Nothing held locally', 'Open a video and it will be fetched as it plays.'),
  );
}

async function loadFeed() {
  const feed = await get('/v1/recommendations?limit=12');
  const cards = feed
    .map((r) => state.byCid.get(r.cid))
    .filter(Boolean)
    .map(videoCard);
  mount(
    $('feed'),
    cards.length
      ? cards
      : empty('☆', 'No recommendations yet', 'Watch something and a model of what you like is built here, on this device.'),
  );
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
    // Nothing new worth recording.
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
  mount(
    $('watch-tags'),
    video.tags.map((t) => el('span', { class: 'tag', text: t })),
  );
  mount($('watch-meta'), [
    el('span', { class: 'badge', text: video.isLocal ? 'published here' : 'from the network' }),
    el('span', { class: 'badge' + (video.haveContent ? ' ok' : ''), text: video.haveContent ? 'held locally' : 'streaming from peers' }),
    video.durationSecs ? el('span', { class: 'badge', text: duration(video.durationSecs) }) : null,
  ]);

  mount($('watch-details'), [
    el('dt', { text: 'Content id' }), el('dd', { class: 'mono', text: video.cid }),
    el('dt', { text: 'Creator' }), el('dd', { class: 'mono', text: video.creator }),
    el('dt', { text: 'Announced' }), el('dd', { text: new Date(video.createdAt * 1000).toLocaleString() }),
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
  player.addEventListener('timeupdate', onTime);
  player.addEventListener('seeked', onSeek);
  player.addEventListener('ended', onEnded);
  player.addEventListener('pause', onPause);
  player.addEventListener('loadedmetadata', onMeta);
  player.addEventListener('error', () => {
    toast('Could not play this video. Its data may not be available from any peer right now.', 'error');
  });

  playerCleanup = () => {
    player.removeEventListener('timeupdate', onTime);
    player.removeEventListener('seeked', onSeek);
    player.removeEventListener('ended', onEnded);
    player.removeEventListener('pause', onPause);
    player.removeEventListener('loadedmetadata', onMeta);
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
  like.textContent = 'Like';
  like.onclick = () => {
    if (!tracker) return;
    tracker.liked = !tracker.liked;
    like.textContent = tracker.liked ? 'Liked' : 'Like';
    like.classList.toggle('primary', tracker.liked);
    tracker.report().then(refreshWhy);
  };

  $('follow').onclick = async () => {
    try {
      await post(`/v1/follow/${video.creator}`);
      toast('Following. Their videos will rank higher for you.');
      refreshWhy();
    } catch (error) {
      reportError(error);
    }
  };

  $('download').onclick = async () => {
    try {
      toast('Fetching every chunk…');
      await post(`/v1/videos/${video.cid}/fetch`, {});
      const result = await post(`/v1/videos/${video.cid}/export`, {});
      toast(`Saved to ${result.path}`);
      await loadVideos();
    } catch (error) {
      reportError(error);
    }
  };

  $('block').onclick = async () => {
    try {
      await post(`/v1/blocked/cids/${video.cid}`, { reason: 'hidden from the viewer' });
      toast('Hidden on this node. Nobody else is affected.');
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
        el('span', { class: 'n', text: reason.detail ? `${reason.factor} · ${reason.detail}` : reason.factor }),
        el('span', {
          class: `v ${reason.value >= 0 ? 'pos' : 'neg'}`,
          text: `${reason.value >= 0 ? '+' : ''}${reason.value.toFixed(3)}`,
        }),
      ]),
    );
    items.push(
      el('li', {}, [
        el('span', { class: 'n', text: 'score' }),
        el('span', { class: 'v', text: explanation.score.toFixed(3) }),
      ]),
    );
    mount($('why'), items);
  } catch {
    mount($('why'), el('li', {}, [el('span', { class: 'n', text: 'No model yet — watch something first.' })]));
  }
}

// ----------------------------------------------------------------- search

async function runSearch(query) {
  $('search-input').value = query;
  if (!query.trim()) {
    mount($('results'), empty('⌕', 'Type something to search'));
    $('search-count').textContent = '';
    return;
  }
  const results = await get(`/v1/search?q=${encodeURIComponent(query)}&limit=60`);
  $('search-count').textContent = `${results.length} result${results.length === 1 ? '' : 's'}`;
  renderGrid($('results'), results, empty('⌕', 'Nothing matched', 'This node can only search what it has already heard about.'));
}

// ------------------------------------------------------------------- boot

function connectionBadge() {
  const badge = $('connection');
  const text = $('connection-text');
  const set = (label, kind) => {
    text.textContent = label;
    badge.className = `badge ${kind}`;
  };

  liveEvents({
    connected: () => set('live', 'ok'),
    disconnected: () => set('offline', 'danger'),
    videoDiscovered: async (event) => {
      toast(`New video: ${event.title}`);
      await loadVideos();
    },
    fetchCompleted: () => loadVideos(),
  });

  poll(async () => {
    const status = await get('/v1/status');
    state.self = status;
    const peers = status.connectedPeers;
    set(peers === 1 ? '1 peer' : `${peers} peers`, peers > 0 ? 'ok' : 'warn');
  }, 5000);
}

async function boot() {
  $('search-form').addEventListener('submit', (event) => {
    event.preventDefault();
    go('search', $('search-input').value);
  });

  router((page, arg) => {
    if (page === 'watch') {
      openWatch(arg).catch(reportError);
      return;
    }
    if (playerCleanup) {
      playerCleanup();
      playerCleanup = null;
    }
    if (page === 'search') runSearch(arg).catch(reportError);
    if (page === 'home') loadFeed().catch(reportError);
  });

  window.addEventListener('pagehide', () => playerCleanup?.());

  try {
    await loadVideos();
    await loadFeed();
  } catch (error) {
    reportError(error);
  }
  connectionBadge();
}

boot();
