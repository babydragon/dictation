/* ==========================================================================
   听写 — 儿童端 PWA
   - 纯原生 JS，无构建步骤，无外部依赖
   - 首页 / 播放 / 完成 / 检查 / 结果 五个状态
   - ?mock=1 进入本地预览（内存假数据 + 合成音频），默认走真实 fetch
   ========================================================================== */
(function () {
  'use strict';

  /* ------------------------------------------------------------- 基础工具 */

  const $ = (id) => document.getElementById(id);
  const MOCK = new URLSearchParams(location.search).has('mock');
  const SVG_NS = 'http://www.w3.org/2000/svg';
  const XLINK_NS = 'http://www.w3.org/1999/xlink';

  function el(tag, props, children) {
    const node = document.createElement(tag);
    if (props) {
      for (const key in props) {
        const value = props[key];
        if (value === null || value === undefined) continue;
        if (key === 'class') node.className = value;
        else if (key === 'text') node.textContent = value;
        else if (key === 'html') node.innerHTML = value;
        else if (key === 'dataset') Object.assign(node.dataset, value);
        else if (key.slice(0, 2) === 'on' && typeof value === 'function') {
          node.addEventListener(key.slice(2), value);
        } else node.setAttribute(key, value);
      }
    }
    if (children) {
      for (const child of [].concat(children)) if (child) node.appendChild(child);
    }
    return node;
  }

  function iconEl(name) {
    const svg = document.createElementNS(SVG_NS, 'svg');
    svg.setAttribute('class', 'icon');
    svg.setAttribute('aria-hidden', 'true');
    svg.setAttribute('focusable', 'false');
    const use = document.createElementNS(SVG_NS, 'use');
    setUse(use, name);
    svg.appendChild(use);
    return svg;
  }

  function setUse(use, name) {
    const href = '#i-' + name;
    use.setAttribute('href', href);
    try { use.setAttributeNS(XLINK_NS, 'xlink:href', href); } catch (e) { /* ignore */ }
  }

  /** 清空并重填（不用 replaceChildren，兼容老 WebView） */
  function setChildren(node, children) {
    while (node.firstChild) node.removeChild(node.firstChild);
    for (const child of [].concat(children || [])) if (child) node.appendChild(child);
  }

  function httpError(status, message) {
    const err = new Error(message);
    err.status = status;
    return err;
  }

  function durationText(ms) {
    const total = Math.max(0, Math.round((ms || 0) / 1000));
    const m = Math.floor(total / 60);
    const s = total % 60;
    if (m <= 0) return s + ' 秒';
    return m + ' 分' + (s ? ' ' + s + ' 秒' : '');
  }

  function approxText(ms) {
    const total = Math.max(0, Math.round((ms || 0) / 1000));
    if (total < 60) return total + ' 秒';
    return Math.round(total / 60) + ' 分钟';
  }

  function todayLabel() {
    const d = new Date();
    const week = ['日', '一', '二', '三', '四', '五', '六'][d.getDay()];
    return d.getMonth() + 1 + ' 月 ' + d.getDate() + ' 日 · 星期' + week;
  }

  /* ------------------------------------------------------- 本地假数据（预览） */

  const MOCK_WORD_MS = 520;
  const mockAudio = new Map();
  const mockToneCache = new Map();
  let mockDue = 2;      // 到期词
  let mockWrong = 0;    // 上次标错、还没纠正的词
  const mockReviewCount = () => mockDue + mockWrong;

  const MOCK_LESSONS = [
    {
      id: 'demo00000001',
      title: '试一试（短课）',
      words: ['摇晃', '粗壮', '毕竟', '旅行'],
    },
    {
      id: 'a1b2c3d4e5f6',
      title: '第5课 词语听写',
      words: [
        '摇晃', '粗壮', '毕竟', '旅行', '准备', '暴雨', '邮票',
        '排列', '规则', '迟到', '勇敢', '整洁', '教室', '朗读',
        '鲜艳', '安静', '丰收', '香甜', 'apple', 'library',
      ],
    },
    {
      id: 'b2c3d4e5f6a1',
      title: '英语 Unit 3',
      words: ['apple', 'library', 'winter', 'picture', 'garden'],
    },
  ];

  const MOCK_REVIEW_WORDS = ['旅行', '整洁'];
  // 课时 session 末尾自动追加的复习词（预览用，贴近服务端行为）
  const MOCK_TAIL_REVIEW = ['整洁'];

  function mockSamples(freq, ms) {
    const sr = 16000;
    const n = Math.round((sr * ms) / 1000);
    const out = new Float32Array(n);
    for (let i = 0; i < n; i++) {
      const t = i / sr;
      const attack = Math.min(1, i / (sr * 0.012));
      const release = Math.min(1, (n - i) / (sr * 0.05));
      out[i] = 0.16 * attack * release *
        (Math.sin(2 * Math.PI * freq * t) + 0.35 * Math.sin(4 * Math.PI * freq * t));
    }
    return out;
  }

  function wavDataUri(samples) {
    const sr = 16000;
    const dataSize = samples.length * 2;
    const buffer = new ArrayBuffer(44 + dataSize);
    const view = new DataView(buffer);
    const writeStr = (offset, str) => {
      for (let i = 0; i < str.length; i++) view.setUint8(offset + i, str.charCodeAt(i));
    };
    writeStr(0, 'RIFF');
    view.setUint32(4, 36 + dataSize, true);
    writeStr(8, 'WAVE');
    writeStr(12, 'fmt ');
    view.setUint32(16, 16, true);
    view.setUint16(20, 1, true);
    view.setUint16(22, 1, true);
    view.setUint32(24, sr, true);
    view.setUint32(28, sr * 2, true);
    view.setUint16(32, 2, true);
    view.setUint16(34, 16, true);
    writeStr(36, 'data');
    view.setUint32(40, dataSize, true);
    for (let i = 0; i < samples.length; i++) {
      const v = Math.max(-1, Math.min(1, samples[i]));
      view.setInt16(44 + i * 2, v < 0 ? v * 0x8000 : v * 0x7fff, true);
    }
    const bytes = new Uint8Array(buffer);
    let binary = '';
    for (let i = 0; i < bytes.length; i += 0x8000) {
      binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    }
    return 'data:audio/wav;base64,' + btoa(binary);
  }

  function mockTone(freq, ms) {
    const key = freq + ':' + ms;
    if (!mockToneCache.has(key)) mockToneCache.set(key, wavDataUri(mockSamples(freq, ms)));
    return mockToneCache.get(key);
  }

  function buildMockManifest(kind, lessonId) {
    const lesson = MOCK_LESSONS.filter((l) => l.id === lessonId)[0];
    if (kind !== 'review' && !lesson) throw httpError(404, '找不到这个课时');

    const words = (kind === 'review' ? MOCK_REVIEW_WORDS : lesson.words).slice();
    // 模拟服务端行为：课时 session 在本课词之后追加错词（item.review=true）
    let reviewFrom = kind === 'review' ? 0 : words.length;
    if (kind !== 'review') words.push(MOCK_TAIL_REVIEW[0]);

    const base = '/media/' + (kind === 'review' ? 'review-src' : lessonId);
    const tailBase = kind === 'review' ? base : '/media/prev-lesson';
    const baseFor = (i) => (i >= reviewFrom ? tailBase : base);
    const gap = 2400;
    const repeat = 2;
    const repeatGap = 700;
    const leadIn = 900;
    const introGap = 700;
    const introMs = 900;
    const outroMs = 800;

    const items = words.map((text, i) => {
      const path = 'clips/' + String(i + 1).padStart(2, '0') + '.wav';
      const itemBase = baseFor(i);
      mockAudio.set(itemBase + '/' + path, mockTone(300 + ((i * 37) % 260), MOCK_WORD_MS));
      return {
        index: i + 1,
        text: text,
        lang: /[\u4e00-\u9fa5]/.test(text) ? 'zh' : 'en',
        base: itemBase,
        clips: [{ path: path, duration_ms: MOCK_WORD_MS }],
        repeat: repeat,
        repeat_gap_ms: repeatGap,
        gap_ms: gap,
        review: kind === 'review' || i >= reviewFrom,
      };
    });

    const intro = kind === 'review' ? null : { path: 'clips/intro.wav', duration_ms: introMs };
    const outro = kind === 'review' ? null : { path: 'clips/outro.wav', duration_ms: outroMs };
    if (intro) mockAudio.set(base + '/' + intro.path, mockTone(520, introMs));
    if (outro) mockAudio.set(base + '/' + outro.path, mockTone(410, outroMs));

    const perItem = repeat * MOCK_WORD_MS + (repeat - 1) * repeatGap + gap;
    const total = leadIn + (intro ? introMs + introGap : 0) + words.length * perItem + (outro ? outroMs : 0);

    return {
      session_id: 'mock-' + Date.now().toString(36),
      kind: kind,
      title: kind === 'review' ? '错词复习' : lesson.title,
      base: base,
      defaults: {
        gap_ms: gap,
        repeat: repeat,
        repeat_gap_ms: repeatGap,
        lead_in_ms: leadIn,
        intro_gap_ms: introGap,
      },
      intro: intro,
      intro_text: intro ? '请准备好纸和笔，听写马上开始。' : undefined,
      outro: outro,
      outro_text: outro ? '听写结束，请检查一遍。' : undefined,
      items: items,
      total_ms: total,
    };
  }

  function mockRequest(path, method, body) {
    return new Promise((resolve, reject) => {
      setTimeout(() => {
        try {
          if (path === '/api/lessons' && method === 'GET') {
            resolve({
              lessons: MOCK_LESSONS.map((l) => ({
                id: l.id,
                title: l.title,
                item_count: l.words.length,
                total_ms: l.words.length * 4500 + 3200,
              })),
            });
            return;
          }
          if (path === '/api/review/summary' && method === 'GET') {
            resolve({ due_count: mockDue, wrong_count: mockWrong, review_count: mockReviewCount() });
            return;
          }
          if (path === '/api/sessions' && method === 'POST') {
            resolve(buildMockManifest('lesson', body && body.lesson_id));
            return;
          }
          if (path === '/api/sessions/review' && method === 'POST') {
            if (mockReviewCount() <= 0) throw httpError(409, '没有到期错词');
            resolve(buildMockManifest('review', null));
            return;
          }
          const m = /^\/api\/sessions\/([^/]+)\/check$/.exec(path);
          if (m && method === 'POST') {
            const results = (body && body.results) || [];
            const wrong = results.filter((r) => !r.correct).length;
            // 刚标错的词当天就算「未纠正」，明天才到期；首页应立刻出现复习入口
            mockDue = 0;
            mockWrong = wrong;
            resolve({ correct: results.length - wrong, wrong: wrong, due_count: mockDue });
            return;
          }
          throw httpError(404, '预览模式里没有这个接口');
        } catch (err) {
          reject(err);
        }
      }, 140);
    });
  }

  /* ------------------------------------------------------------------ API */

  function defaultStatusMessage(status) {
    if (status === 404) return '找不到这一课';
    if (status === 409) return '这会儿做不了，稍后再试';
    if (status === 0) return '连不上服务，检查一下网络';
    return '请求失败（' + status + '）';
  }

  async function request(path, opts) {
    const options = opts || {};
    const method = options.method || 'GET';
    if (MOCK) return mockRequest(path, method, options.body);

    const init = { method: method, headers: {} };
    if (options.body !== undefined) {
      init.headers['Content-Type'] = 'application/json';
      init.body = JSON.stringify(options.body);
    }

    let res;
    try {
      res = await fetch(path, init);
    } catch (err) {
      throw httpError(0, defaultStatusMessage(0));
    }

    let data = null;
    const text = await res.text();
    if (text) {
      try { data = JSON.parse(text); } catch (err) { data = null; }
    }
    if (!res.ok) {
      throw httpError(res.status, (data && data.error) || defaultStatusMessage(res.status));
    }
    return data;
  }

  const API = {
    lessons: () => request('/api/lessons'),
    reviewSummary: () => request('/api/review/summary'),
    createSession: (lessonId) => request('/api/sessions', { method: 'POST', body: { lesson_id: lessonId } }),
    createReview: () => request('/api/sessions/review', { method: 'POST', body: {} }),
    check: (sessionId, results) =>
      request('/api/sessions/' + encodeURIComponent(sessionId) + '/check', {
        method: 'POST',
        body: { results: results },
      }),
  };

  /* --------------------------------------------------------------- 界面状态 */

  const SCREENS = ['home', 'player', 'done', 'check', 'result', 'error'];
  const LAST_LESSON_KEY = 'dictation.lastLesson';

  const state = {
    lessons: [],
    reviewCount: 0,
    marked: new Set(),
    busy: false,
    submitting: false,
    screen: 'home',
  };

  let els = {};

  function showScreen(name) {
    if (state.screen === name) return;
    SCREENS.forEach((s) => {
      const node = els['screen-' + s];
      if (node) node.hidden = s !== name;
    });
    state.screen = name;
    try { window.scrollTo(0, 0); } catch (e) { /* ignore */ }
  }

  /* ------------------------------------------------------------------ 弹层 */

  let modalResolve = null;

  function confirmDialog(opts) {
    return new Promise((resolve) => {
      els.modalTitle.textContent = opts.title || '确定吗？';
      els.modalText.textContent = opts.text || '';
      els.modalConfirm.textContent = opts.confirmLabel || '确定';
      els.modalCancel.textContent = opts.cancelLabel || '取消';
      els.modal.hidden = false;
      modalResolve = resolve;
      setTimeout(() => {
        try { els.modalCancel.focus({ preventScroll: true }); } catch (e) { /* ignore */ }
      }, 30);
    });
  }

  function closeModal(value) {
    if (!modalResolve) return;
    const resolve = modalResolve;
    modalResolve = null;
    els.modal.hidden = true;
    resolve(value);
  }

  let toastTimer = null;
  function toast(message) {
    els.toast.textContent = message;
    els.toast.hidden = false;
    els.toast.classList.toggle('toast-raised', state.screen === 'check');
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { els.toast.hidden = true; toastTimer = null; }, 3600);
  }

  /* ------------------------------------------------ 返回拦截（误触保护） */

  const guard = { active: false, title: '', text: '', confirmLabel: '', cancelLabel: '', onLeave: null, pushed: false };
  let bypassPop = false;

  function armGuard(opts) {
    if (guard.active) return;
    guard.active = true;
    guard.title = opts.title || '确定离开吗？';
    guard.text = opts.text || '';
    guard.confirmLabel = opts.confirmLabel || '离开';
    guard.cancelLabel = opts.cancelLabel || '继续';
    guard.onLeave = opts.onLeave || goHome;
    try {
      history.pushState({ dictationGuard: 1 }, '');
      guard.pushed = true;
    } catch (e) {
      guard.pushed = false;
    }
  }

  function disarmGuard() {
    if (!guard.active) return;
    guard.active = false;
    if (!guard.pushed) return;
    guard.pushed = false;
    try {
      if (history.state && history.state.dictationGuard) {
        bypassPop = true;
        history.back();
      }
    } catch (e) { /* ignore */ }
  }

  window.addEventListener('popstate', () => {
    if (bypassPop) { bypassPop = false; return; }
    if (!guard.active) return;
    try { history.pushState({ dictationGuard: 1 }, ''); } catch (e) { /* ignore */ }
    askLeave();
  });

  async function askLeave() {
    if (!guard.active) return;
    // 确认框打开时先停住，避免孩子在犹豫时音频还在走
    const pauseOnOpen = state.screen === 'player' && player.running && !player.paused;
    if (pauseOnOpen) togglePause();

    const ok = await confirmDialog({
      title: guard.title,
      text: guard.text,
      confirmLabel: guard.confirmLabel,
      cancelLabel: guard.cancelLabel,
    });

    if (!ok) {
      if (pauseOnOpen && player.running && player.paused) togglePause();
      return;
    }
    const leave = guard.onLeave;
    disarmGuard();
    if (leave) leave();
  }

  /* ------------------------------------------------------------------ 音频 */

  const audio = $('audio');
  let audioUnlocked = false;
  let previewBtn = null;

  function unlockAudio() {
    if (audioUnlocked) return;
    audioUnlocked = true;
    try {
      audio.src = wavDataUri(new Float32Array(160));
      const p = audio.play();
      if (p && p.then) {
        p.then(() => { try { audio.pause(); } catch (e) { /* ignore */ } })
          .catch(() => { /* 解锁失败就等首次 play 再试 */ });
      }
    } catch (e) { /* ignore */ }
  }

  function mediaUrl(base, path, durationMs) {
    if (!path) return null;
    const url = String(base || '').replace(/\/+$/, '') + '/' + path;
    if (!MOCK) return url;
    if (!mockAudio.has(url)) {
      // 刷新后 mockAudio 已清空：按 URL 生成等长音，保证预览也能续听
      mockAudio.set(url, mockTone(mockFreq(url), Number(durationMs) || MOCK_WORD_MS));
    }
    return mockAudio.get(url);
  }

  function mockFreq(seed) {
    let hash = 0;
    for (let i = 0; i < seed.length; i++) hash = (hash * 31 + seed.charCodeAt(i)) % 997;
    return 300 + (hash % 260);
  }

  function clipSrc(item) {
    if (!item || !item.clips || !item.clips.length) return null;
    const base = item.base || (player.manifest && player.manifest.base) || '';
    const clip = item.clips[0];
    return mediaUrl(base, clip.path, clip.duration_ms);
  }

  /* --------------------------------------------------------------- 播放引擎 */

  const player = {
    manifest: null,
    runId: 0,
    running: false,
    paused: false,
    index: 0,
    item: null,
    clipFinish: null,
    replayRequested: false,
    activeMs: 0,
    segStart: 0,
  };

  function alive(runId) {
    return player.runId === runId;
  }

  /** 可暂停的计时器：每 50ms 检查一次，暂停时不扣时间。 */
  function sleep(ms, runId) {
    return new Promise((resolve) => {
      if (!alive(runId)) return resolve({ type: 'stop', remaining: 0 });
      let remaining = Math.max(0, ms || 0);
      let last = performance.now();
      const tick = () => {
        if (!alive(runId)) return resolve({ type: 'stop', remaining: remaining });
        if (player.replayRequested) {
          player.replayRequested = false;
          return resolve({ type: 'replay', remaining: remaining });
        }
        const now = performance.now();
        if (!player.paused) remaining -= now - last;
        last = now;
        if (remaining <= 0) return resolve({ type: 'ok', remaining: 0 });
        setTimeout(tick, 50);
      };
      setTimeout(tick, 50);
    });
  }

  /** 播一条音频；resolve ended / error / replay（被打断去重听）/ stop */
  function playUrl(src, runId) {
    return new Promise((resolve) => {
      if (!alive(runId)) return resolve({ type: 'stop' });
      if (!src) return resolve({ type: 'ended' });

      let settled = false;
      const finish = (type) => {
        if (settled) return;
        settled = true;
        if (player.clipFinish === finish) player.clipFinish = null;
        audio.removeEventListener('ended', onEnded);
        audio.removeEventListener('error', onError);
        resolve({ type: type });
      };
      const onEnded = () => finish('ended');
      const onError = () => finish('error');

      player.clipFinish = finish;
      audio.addEventListener('ended', onEnded);
      audio.addEventListener('error', onError);

      try {
        audio.src = src;
        audio.currentTime = 0;
      } catch (e) { /* ignore */ }

      const p = audio.play();
      if (p && typeof p.catch === 'function') {
        p.catch((err) => {
          if (err && err.name === 'AbortError') return;
          finish('error');
        });
      }
    });
  }

  async function waitWhilePaused(runId) {
    while (player.paused && alive(runId)) {
      await new Promise((r) => setTimeout(r, 80));
    }
    return alive(runId);
  }

  async function replayItem(item, runId) {
    if (!item) return true;
    player.replayRequested = false;
    setStatus('再听一遍');
    const res = await playUrl(clipSrc(item), runId);
    if (res.type === 'stop') return false;
    if (res.type === 'error') { showAudioError(); return false; }
    if (res.type === 'replay') return replayItem(item, runId);
    setStatus(player.paused ? '已暂停' : '正在听写');
    return true;
  }

  async function sleepWithReplay(ms, runId, item) {
    let remaining = Math.max(0, ms || 0);
    for (;;) {
      const res = await sleep(remaining, runId);
      if (res.type === 'stop') return 'stop';
      if (res.type === 'replay') {
        remaining = res.remaining;
        const ok = await replayItem(item, runId);
        if (!ok) return 'stop';
        continue;
      }
      return 'ok';
    }
  }

  function num() {
    for (let i = 0; i < arguments.length; i++) {
      const v = arguments[i];
      if (typeof v === 'number' && isFinite(v)) return v;
    }
    return 0;
  }

  async function runTimeline(runId, startIndex) {
    const m = player.manifest;
    if (!m) return;
    const d = m.defaults || {};
    const items = m.items || [];
    const total = items.length;
    if (!total) { finishPlayback(); return; }

    let start = Math.floor(Number(startIndex) || 0);
    if (!isFinite(start) || start < 0 || start >= total) start = 0;

    if (start === 0) {
      const leadIn = num(d.lead_in_ms, 3000);
      const introGap = num(d.intro_gap_ms, 1500);

      // 1. 预备静音
      const lead = await sleep(leadIn, runId);
      if (lead.type === 'stop') return;

      // 2. 开场白
      if (m.intro) {
        setStatus('准备中…');
        const r = await playUrl(mediaUrl(m.base, m.intro.path, m.intro.duration_ms), runId);
        if (r.type === 'stop') return;
        if (r.type === 'error') return showAudioError();
        if (introGap > 0) {
          const gap = await sleep(introGap, runId);
          if (gap.type === 'stop') return;
        }
      }
    } else {
      // 3. 续听：跳过 lead_in / intro / intro_gap，只留一点缓冲
      setStatus('继续上次…');
      const buffer = await sleep(RESUME_LEAD_MS, runId);
      if (buffer.type === 'stop') return;
    }

    // 4. 逐词
    for (let i = start; i < total; i++) {
      if (!alive(runId)) return;
      const item = items[i];
      player.index = i;
      player.item = item;
      renderProgress(i, total);
      setReplayEnabled(true);
      setStatus('正在听写');
      saveResume(m, i);
      if (!(await waitWhilePaused(runId))) return;

      const repeat = Math.max(1, Math.round(num(item.repeat, d.repeat, 1)));
      const repeatGap = num(item.repeat_gap_ms, d.repeat_gap_ms, 1200);
      const gap = num(item.gap_ms, d.gap_ms, 0);
      const src = clipSrc(item);

      for (let r = 0; r < repeat; r++) {
        if (!alive(runId)) return;
        if (!(await waitWhilePaused(runId))) return;

        const res = await playUrl(src, runId);
        if (res.type === 'stop') return;
        if (res.type === 'error') return showAudioError();
        if (res.type === 'replay') {
          const ok = await replayItem(item, runId);
          if (!ok) return;
        }
        if (r < repeat - 1) {
          const s = await sleepWithReplay(repeatGap, runId, item);
          if (s === 'stop') return;
        }
      }

      const g = await sleepWithReplay(gap, runId, item);
      if (g === 'stop') return;
    }

    // 4. 结束语
    player.item = null;
    setReplayEnabled(false);
    if (m.outro) {
      setStatus('结束语');
      const r = await playUrl(mediaUrl(m.base, m.outro.path, m.outro.duration_ms), runId);
      if (r.type === 'stop') return;
      if (r.type === 'error') return showAudioError();
    }

    finishPlayback();
  }

  function renderPlayerChrome(manifest, startIndex) {
    const items = manifest.items || [];
    const start = Math.max(0, Math.min(Number(startIndex) || 0, Math.max(0, items.length - 1)));
    els.playerTitle.textContent = manifest.title || '';
    els.counterTotal.textContent = String(items.length);
    els.counterNow.textContent = items.length ? String(start + 1) : '0';

    const frag = document.createDocumentFragment();
    for (let i = 0; i < items.length; i++) frag.appendChild(el('span', { class: 'seg' }));
    setChildren(els.segments, frag);

    els.btnToggleLabel.textContent = '暂停';
    setUse(els.btnToggleUse, 'pause');
    setReplayEnabled(false);
  }

  function renderProgress(index, total) {
    els.counterNow.textContent = String(Math.min(index + 1, total));
    const segs = els.segments.children;
    for (let i = 0; i < segs.length; i++) {
      segs[i].className = 'seg' + (i < index ? ' done' : i === index ? ' now' : '');
    }
  }

  function setStatus(text) {
    if (els.playerStatusText.textContent !== text) els.playerStatusText.textContent = text;
  }

  function setReplayEnabled(on) {
    els.btnReplay.disabled = !on;
  }

  function startPlayback(manifest, startIndex) {
    const items = (manifest && manifest.items) || [];
    let start = Math.floor(Number(startIndex) || 0);
    if (!isFinite(start) || start < 0 || start >= items.length) start = 0;

    stopPlayback();
    player.manifest = manifest;
    state.marked = new Set();
    player.runId += 1;
    player.running = true;
    player.paused = false;
    player.index = start;
    player.item = null;
    player.activeMs = 0;
    player.segStart = performance.now();
    player.replayRequested = false;

    renderPlayerChrome(manifest, start);
    if (start > 0) renderProgress(start, items.length);
    setStatus(start > 0 ? '继续上次…' : '准备中…');
    els['screen-player'].classList.remove('is-paused');
    showScreen('player');
    requestWakeLock();
    saveResume(manifest, start);

    armGuard({
      title: '要回首页吗？',
      text: start > 0 ? '这次还没听完，回首页后可以接着听。' : '这次还没听完，回去就从头开始了。',
      confirmLabel: '回首页',
      cancelLabel: '继续听写',
    });

    const runId = player.runId;
    runTimeline(runId, start).catch((err) => {
      console.error(err);
      showAudioError();
    });
  }

  function stopPlayback() {
    player.runId += 1;
    if (player.clipFinish) {
      const finish = player.clipFinish;
      player.clipFinish = null;
      finish('stop');
    }
    player.running = false;
    player.paused = false;
    player.item = null;
    player.replayRequested = false;
    try { audio.pause(); } catch (e) { /* ignore */ }
    releaseWakeLock();
  }

  function finishPlayback() {
    clearResume();
    if (!player.paused) player.activeMs += performance.now() - player.segStart;
    player.running = false;
    player.paused = false;
    player.item = null;
    releaseWakeLock();
    disarmGuard();

    const count = (player.manifest && player.manifest.items ? player.manifest.items.length : 0);
    els.doneMeta.textContent = count + ' 个词 · 用时 ' + durationText(player.activeMs);
    showScreen('done');
  }

  function togglePause() {
    if (!player.running) return;
    if (!player.paused) {
      player.activeMs += performance.now() - player.segStart;
      player.paused = true;
      try { audio.pause(); } catch (e) { /* ignore */ }
      saveCurrentResume();
    } else {
      player.paused = false;
      player.segStart = performance.now();
      if (player.clipFinish) {
        const p = audio.play();
        if (p && typeof p.catch === 'function') p.catch(() => { /* ignore */ });
      }
    }
    updatePauseUI();
  }

  function updatePauseUI() {
    const paused = player.paused;
    els['screen-player'].classList.toggle('is-paused', paused);
    els.btnToggleLabel.textContent = paused ? '继续' : '暂停';
    setUse(els.btnToggleUse, paused ? 'play' : 'pause');
    if (player.running) setStatus(paused ? '已暂停' : (player.item ? '正在听写' : '准备中…'));
  }

  function requestReplay() {
    if (!player.running || !player.item) return;
    player.replayRequested = true;
    if (player.clipFinish) player.clipFinish('replay');
  }

  function showAudioError() {
    clearResume();
    stopPlayback();
    disarmGuard();
    showScreen('error');
  }

  /* ------------------------------------------------------- 断点续听（本地） */

  // 存在 localStorage，隐私模式下写入失败会静默忽略
  const RESUME_KEY = 'dictation.resume.v1';
  const RESUME_TTL_MS = 7 * 24 * 60 * 60 * 1000;
  const RESUME_LEAD_MS = 300;

  function clearResume() {
    try { localStorage.removeItem(RESUME_KEY); } catch (e) { /* ignore */ }
  }

  /** { sessionId, manifest, index, savedAt }；index 从 0 开始 */
  function saveResume(manifest, index) {
    const items = manifest && manifest.items;
    if (!items || !items.length) return;
    let safeIndex = Math.floor(Number(index) || 0);
    if (!isFinite(safeIndex) || safeIndex < 0) safeIndex = 0;
    if (safeIndex >= items.length) safeIndex = items.length - 1;

    const payload = {
      sessionId: String(manifest.session_id || ''),
      manifest: manifest,
      index: safeIndex,
      savedAt: Date.now(),
    };
    try {
      localStorage.setItem(RESUME_KEY, JSON.stringify(payload));
    } catch (e) {
      /* 隐私模式 / 配额不足：不影响播放 */
    }
  }

  function readResume() {
    let raw = null;
    try { raw = localStorage.getItem(RESUME_KEY); } catch (e) { return null; }
    if (!raw) return null;

    let data = null;
    try { data = JSON.parse(raw); } catch (e) { data = null; }

    const manifest = data && data.manifest;
    const items = manifest && manifest.items;
    const index = data ? Math.floor(Number(data.index)) : NaN;
    const savedAt = data ? Number(data.savedAt) : NaN;

    const valid =
      !!data && typeof manifest === 'object' && manifest !== null &&
      Array.isArray(items) && items.length > 0 &&
      isFinite(index) && index >= 0 && index < items.length &&
      isFinite(savedAt) && savedAt > 0;

    if (!valid) {
      clearResume();
      return null;
    }
    if (Date.now() - savedAt > RESUME_TTL_MS) {
      clearResume();
      return null;
    }

    return {
      sessionId: String(data.sessionId || manifest.session_id || ''),
      manifest: manifest,
      index: index,
      savedAt: savedAt,
    };
  }

  function saveCurrentResume() {
    if (!player.running || !player.manifest) return;
    saveResume(player.manifest, player.index);
  }

  function timeAgo(ts) {
    const diff = Math.max(0, Date.now() - ts);
    const minutes = Math.floor(diff / 60000);
    if (minutes < 1) return '刚刚';
    if (minutes < 60) return minutes + ' 分钟前';
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return hours + ' 小时前';
    return Math.floor(hours / 24) + ' 天前';
  }

  /* --------------------------------------------------------------- 首页渲染 */

  function readLastLesson() {
    try { return localStorage.getItem(LAST_LESSON_KEY); } catch (e) { return null; }
  }

  function rememberLesson(id) {
    try { localStorage.setItem(LAST_LESSON_KEY, id); } catch (e) { /* ignore */ }
  }

  function reviewCard(count) {
    const card = el('button', { class: 'card card-review', type: 'button' }, [
      el('span', { class: 'review-count', text: String(count) }),
      el('div', { class: 'card-main' }, [
        el('p', { class: 'card-title', text: '错词复习' }),
        el('p', { class: 'card-meta', text: '共 ' + count + ' 个词，再听一遍' }),
      ]),
      el('span', { class: 'chev' }, [iconEl('chev')]),
    ]);
    card.addEventListener('click', () => startReview(card));
    return card;
  }

  function resumeCard(data) {
    const items = data.manifest.items || [];
    const total = items.length;
    const title = data.manifest.title || '上次的听写';
    const position = Math.min(data.index + 1, total);

    const main = el('button', { class: 'resume-main', type: 'button' }, [
      el('span', { class: 'resume-badge', 'aria-hidden': 'true' }, [iconEl('play')]),
      el('span', { class: 'resume-body' }, [
        el('span', { class: 'resume-top' }, [
          el('span', { class: 'resume-title', text: '继续上次听写' }),
          el('span', { class: 'resume-time', text: timeAgo(data.savedAt) }),
        ]),
        el('span', { class: 'resume-meta', text: title + ' · ' + position + ' / ' + total }),
      ]),
      el('span', { class: 'chev' }, [iconEl('chev')]),
    ]);
    main.addEventListener('click', () => {
      unlockAudio();
      startPlayback(data.manifest, data.index);
    });

    const dismiss = el('button', { class: 'resume-dismiss', type: 'button', text: '不继续' });
    dismiss.addEventListener('click', () => dismissResume());

    return el('div', { class: 'card-resume' }, [main, dismiss]);
  }

  async function dismissResume() {
    const ok = await confirmDialog({
      title: '不继续这次听写？',
      text: '上次听到的进度会被清除，下次会从头开始。',
      confirmLabel: '清除进度',
      cancelLabel: '留着',
    });
    if (!ok) return;
    clearResume();
    renderHome();
    toast('已清除上次进度');
  }

  function lessonCard(lesson, isLast) {
    const count = Number(lesson.item_count || 0);
    const meta = [];
    if (isLast) meta.push('上次');
    meta.push(count + ' 个词');
    if (lesson.total_ms) meta.push('约 ' + approxText(lesson.total_ms));

    const card = el('button', { class: 'card', type: 'button' }, [
      el('div', { class: 'card-main' }, [
        el('p', { class: 'card-title', text: lesson.title || '未命名课时' }),
        el('p', { class: 'card-meta', text: meta.join(' · ') }),
      ]),
      el('span', { class: 'chev' }, [iconEl('chev')]),
    ]);
    card.addEventListener('click', () => startLesson(lesson, card));
    return card;
  }

  function emptyState() {
    return el('div', { class: 'empty' }, [
      el('p', {
        html: '还没有课时，先用 <code>dictation gen</code> 生成一课，刷新后就出现在这里。',
      }),
    ]);
  }

  function homeError(err) {
    const retry = el('button', { class: 'btn btn-secondary btn-block', type: 'button' }, [
      iconEl('retry'),
      el('span', { text: '重试' }),
    ]);
    retry.addEventListener('click', () => loadHome());
    return [
      el('div', { class: 'empty' }, [
        el('p', { html: '<strong>读不到课时列表。</strong>' }),
        el('p', { text: (err && err.message) || '服务可能没启动，或者网络断了。' }),
      ]),
      retry,
    ];
  }

  function renderHome() {
    const frag = document.createDocumentFragment();
    const resume = readResume();

    if (state.reviewCount > 0) frag.appendChild(reviewCard(state.reviewCount));
    if (resume) frag.appendChild(resumeCard(resume));

    frag.appendChild(el('p', { class: 'section-label', text: '课时' }));

    if (!state.lessons.length) {
      frag.appendChild(emptyState());
    } else {
      const last = readLastLesson();
      state.lessons.forEach((lesson) => {
        frag.appendChild(lessonCard(lesson, lesson.id === last));
      });
    }

    setChildren(els.homeBody, frag);
  }

  async function loadHome() {
    disarmGuard();
    stopPlayback();
    if (previewBtn) {
      previewBtn.classList.remove('is-playing');
      previewBtn = null;
    }
    showScreen('home');
    setChildren(els.homeBody, el('div', { class: 'loading-block', html: '<span class="spinner"></span>正在读取…' }));

    const [lessonsRes, summaryRes] = await Promise.allSettled([API.lessons(), API.reviewSummary()]);

    if (lessonsRes.status === 'rejected') {
      const frag = document.createDocumentFragment();
      const resume = readResume();
      if (resume) frag.appendChild(resumeCard(resume));
      homeError(lessonsRes.reason).forEach((node) => frag.appendChild(node));
      setChildren(els.homeBody, frag);
      return;
    }
    state.lessons = (lessonsRes.value && lessonsRes.value.lessons) || [];
    const summary = summaryRes.status === 'fulfilled' && summaryRes.value ? summaryRes.value : null;
    // review_count = 未纠正错词 + 到期词（当天标错的词也能立刻复习）；老服务端只有 due_count
    const hasReviewCount = summary && summary.review_count !== null && summary.review_count !== undefined;
    const reviewTotal = summary ? (hasReviewCount ? summary.review_count : summary.due_count) : 0;
    state.reviewCount = Math.max(0, Number(reviewTotal) || 0);
    renderHome();
  }

  function goHome() {
    disarmGuard();
    stopPlayback();
    loadHome();
  }

  /* --------------------------------------------------------------- 进入播放 */

  async function startLesson(lesson, card) {
    if (state.busy) return;
    state.busy = true;
    card.disabled = true;
    unlockAudio();
    try {
      const manifest = await API.createSession(lesson.id);
      state.busy = false;
      card.disabled = false;
      rememberLesson(lesson.id);
      startPlayback(manifest);
    } catch (err) {
      state.busy = false;
      card.disabled = false;
      toast((err && err.message) || '没法开始，再试一次');
    }
  }

  async function startReview(card) {
    if (state.busy) return;
    state.busy = true;
    card.disabled = true;
    unlockAudio();
    try {
      const manifest = await API.createReview();
      state.busy = false;
      card.disabled = false;
      startPlayback(manifest);
    } catch (err) {
      state.busy = false;
      card.disabled = false;
      toast((err && err.message) || '没法开始，再试一次');
      if (err && err.status === 409) loadHome();
    }
  }

  /* ----------------------------------------------------------------- 检查 */

  function updateCheckCount() {
    els.checkCount.textContent = String(state.marked.size);
  }

  function toggleMark(index, row, btn) {
    const wasMarked = state.marked.has(index);
    if (wasMarked) state.marked.delete(index);
    else state.marked.add(index);

    const marked = !wasMarked;
    row.classList.toggle('is-wrong', marked);
    btn.setAttribute('aria-pressed', marked ? 'true' : 'false');
    btn.textContent = marked ? '已标错' : '标错';
    updateCheckCount();
  }

  function playPreview(item, btn) {
    const src = clipSrc(item);
    if (!src) return;
    try {
      audio.pause();
      audio.src = src;
      audio.currentTime = 0;
      const p = audio.play();
      if (p && typeof p.catch === 'function') p.catch(() => { /* ignore */ });
    } catch (e) { /* ignore */ }

    if (previewBtn && previewBtn !== btn) previewBtn.classList.remove('is-playing');
    previewBtn = btn;
    btn.classList.add('is-playing');
  }

  function checkRow(item, index) {
    const row = el('li', { class: 'check-row' });
    const soundBtn = el('button', {
      class: 'icon-btn',
      type: 'button',
      'aria-label': '再听一遍 ' + item.text,
    }, [iconEl('sound')]);
    const markBtn = el('button', {
      class: 'mark-btn',
      type: 'button',
      'aria-pressed': 'false',
      text: '标错',
    });

    soundBtn.addEventListener('click', () => playPreview(item, soundBtn));
    markBtn.addEventListener('click', () => toggleMark(index, row, markBtn));

    // 课时 session 里自动追加的错词带 review=true，标一个小标签区分（不占序号、不影响按钮）
    const wordCell = el('div', { class: 'check-word-cell' }, [
      el('p', { class: 'check-word', text: item.text }),
      item.review ? el('span', { class: 'tag-review', text: '复习' }) : null,
    ]);

    row.append(
      el('span', { class: 'check-no', text: String(item.index || index + 1) }),
      wordCell,
      el('div', { class: 'check-tools' }, [soundBtn, markBtn])
    );
    return row;
  }

  function enterCheck() {
    const manifest = player.manifest;
    if (!manifest || !manifest.items || !manifest.items.length) {
      goHome();
      return;
    }
    stopPlayback();
    state.marked = new Set();
    state.submitting = false;

    const frag = document.createDocumentFragment();
    manifest.items.forEach((item, index) => frag.appendChild(checkRow(item, index)));
    setChildren(els.checkList, frag);
    updateCheckCount();

    els.btnSubmit.disabled = false;
    els.btnSubmit.textContent = '完成检查';

    showScreen('check');
    armGuard({
      title: '还没提交检查',
      text: '离开的话，这次标的错不会保存。',
      confirmLabel: '回首页',
      cancelLabel: '继续检查',
      onLeave: goHome,
    });
  }

  async function submitCheck() {
    const manifest = player.manifest;
    if (!manifest || !manifest.items || state.submitting) return;

    const results = manifest.items.map((item, index) => ({
      position: index + 1,
      correct: !state.marked.has(index),
    }));

    state.submitting = true;
    els.btnSubmit.disabled = true;
    els.btnSubmit.textContent = '提交中…';

    try {
      const data = await API.check(manifest.session_id, results);
      state.submitting = false;
      clearResume();
      showResult(data);
    } catch (err) {
      state.submitting = false;
      els.btnSubmit.disabled = false;
      els.btnSubmit.textContent = '完成检查';
      if (err && err.status === 409) {
        toast('这节课已经提交过了，回首页吧');
      } else {
        toast('没提交成功，标记还在，再点一次就好');
      }
      if (window.console) console.warn('check failed', err);
    }
  }

  function showResult(data) {
    const manifest = player.manifest;
    const correct = Number((data && data.correct) || 0);
    const wrong = Number((data && data.wrong) || 0);

    els.resultCorrect.textContent = String(correct);
    els.resultWrong.textContent = String(wrong);
    els.resultNote.textContent = wrong > 0
      ? '错词已加入复习，下次会再听一遍。'
      : '一个都没错，写得很稳。';

    const chips = [];
    if (manifest && manifest.items) {
      manifest.items.forEach((item, index) => {
        if (state.marked.has(index)) chips.push(el('span', { class: 'chip', text: item.text }));
      });
    }
    setChildren(els.resultWords, chips);

    disarmGuard();
    showScreen('result');
  }

  function skipCheck() {
    stopPlayback();
    disarmGuard();
    goHome();
  }

  /* --------------------------------------------------------------- Wake Lock */

  let wakeLock = null;

  async function requestWakeLock() {
    try {
      if (!('wakeLock' in navigator) || !navigator.wakeLock) return;
      if (wakeLock) return;
      wakeLock = await navigator.wakeLock.request('screen');
      wakeLock.addEventListener('release', () => { wakeLock = null; });
    } catch (e) {
      wakeLock = null;
    }
  }

  function releaseWakeLock() {
    if (!wakeLock) return;
    try { wakeLock.release(); } catch (e) { /* ignore */ }
    wakeLock = null;
  }

  /* ---------------------------------------------------------------- 事件绑定 */

  function bindEvents() {
    els.btnToggle.addEventListener('click', togglePause);
    els.btnReplay.addEventListener('click', requestReplay);

    els.playerBack.addEventListener('click', () => {
      askLeave();
    });

    els.btnCheck.addEventListener('click', enterCheck);
    els.btnAgain.addEventListener('click', () => {
      if (!player.manifest) return goHome();
      unlockAudio();
      startPlayback(player.manifest);
    });
    els.btnDoneHome.addEventListener('click', goHome);

    els.btnSubmit.addEventListener('click', submitCheck);
    els.btnSkip.addEventListener('click', skipCheck);

    els.btnResultHome.addEventListener('click', goHome);
    els.btnErrorHome.addEventListener('click', goHome);

    els.modalConfirm.addEventListener('click', () => closeModal(true));
    els.modalCancel.addEventListener('click', () => closeModal(false));
    els.modal.addEventListener('click', (event) => {
      if (event.target === els.modal) closeModal(false);
    });

    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape' && !els.modal.hidden) closeModal(false);
    });

    audio.addEventListener('ended', () => {
      if (previewBtn) {
        previewBtn.classList.remove('is-playing');
        previewBtn = null;
      }
    });

    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') {
        saveCurrentResume();
        if (player.running && !player.paused) togglePause();
      } else if (player.running && !player.paused) {
        requestWakeLock();
      }
    });

    window.addEventListener('pagehide', saveCurrentResume);

    document.addEventListener('pointerdown', unlockAudio, { once: true });
  }

  /* -------------------------------------------------------------------- 启动 */

  function boot() {
    els = {
      'home-date': $('home-date'),
      homeBody: $('home-body'),
      'mock-badge': $('mock-badge'),
      'screen-home': $('screen-home'),
      'screen-player': $('screen-player'),
      'screen-done': $('screen-done'),
      'screen-check': $('screen-check'),
      'screen-result': $('screen-result'),
      'screen-error': $('screen-error'),

      playerTitle: $('player-title'),
      playerStatusText: $('player-status-text'),
      playerBack: $('player-back'),
      counterNow: $('counter-now'),
      counterTotal: $('counter-total'),
      segments: $('segments'),

      btnToggle: $('btn-toggle'),
      btnToggleLabel: $('btn-toggle-label'),
      btnToggleUse: $('btn-toggle').querySelector('use'),
      btnReplay: $('btn-replay'),

      doneMeta: $('done-meta'),
      btnCheck: $('btn-check'),
      btnAgain: $('btn-again'),
      btnDoneHome: $('btn-done-home'),

      checkList: $('check-list'),
      checkCount: $('check-count'),
      btnSubmit: $('btn-submit'),
      btnSkip: $('btn-skip'),

      resultCorrect: $('result-correct'),
      resultWrong: $('result-wrong'),
      resultNote: $('result-note'),
      resultWords: $('result-words'),
      btnResultHome: $('btn-result-home'),

      btnErrorHome: $('btn-error-home'),

      modal: $('modal'),
      modalTitle: $('modal-title'),
      modalText: $('modal-text'),
      modalConfirm: $('modal-confirm'),
      modalCancel: $('modal-cancel'),

      toast: $('toast'),
    };

    els['mock-badge'].hidden = !MOCK;
    els['home-date'].textContent = todayLabel();

    bindEvents();
    loadHome();
  }

  boot();
})();
