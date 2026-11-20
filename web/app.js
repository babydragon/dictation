/* ==========================================================================
   听写 — 儿童端 PWA
   - 纯原生 JS，无构建步骤，无外部依赖
   - 首页 / 播放 / 完成 / 检查 / 结果 五个状态
   - 两种模式共用一份代码：server（本地 serve，同源 /api）与
     static（Cloudflare Pages 壳 + R2 只读数据），由 config.js 决定
   - ?mock=1 进入本地预览（内存假数据 + 合成音频），默认走真实 fetch
   ========================================================================== */
(function () {
  'use strict';

  /* ------------------------------------------------------------- 基础工具 */

  const $ = (id) => document.getElementById(id);
  const MOCK = new URLSearchParams(location.search).has('mock');
  const SVG_NS = 'http://www.w3.org/2000/svg';
  const XLINK_NS = 'http://www.w3.org/1999/xlink';

  /* ------------------------------------------------------- 运行模式与数据源 */

  /**
   * 配置由 <script src="config.js"> 提供；缺失、解析失败或 mode 非 static
   * 一律按 server 处理，不做隐式降级试探。
   */
  const CONFIG = (function () {
    const raw = window.DICTATION_CONFIG;
    if (!raw || typeof raw !== 'object') return { mode: 'server' };
    if (String(raw.mode || '') !== 'static') return { mode: 'server' };
    return { mode: 'static', dataBase: String(raw.dataBase || '') };
  })();
  const STATIC = CONFIG.mode === 'static';

  /**
   * 访问只读数据源的唯一出口。v1 没有口令，原样返回；
   * 以后加口令只改这里（追加 ?k=），不要在其他地方拼 URL。
   * base 必须保持干净，否则会写进断点续听快照。
   */
  function dataUrl(path) {
    if (!CONFIG.dataBase) return path;
    const base = CONFIG.dataBase.replace(/\/+$/, '') + '/';
    try {
      // 允许 dataBase 写成同源相对路径（如 "/data"）
      return new URL(path, new URL(base, location.href)).href;
    } catch (e) {
      return base + path;
    }
  }

  /** 静态模式没有的入口，一律不渲染，而不是点了才报错。 */
  const CAP = {
    review: !STATIC,
    lessonReview: !STATIC,
    regenerate: !STATIC,
    sessionLink: !STATIC,
    importResult: !STATIC,
  };

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
  // 预览里把待复习词都算到第 5 课头上，用来演示「课内复习」入口
  const MOCK_REVIEW_LESSON_ID = 'a1b2c3d4e5f6';

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

    const base = '/media/' + (lessonId || 'review-src');
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
      title: kind === 'review'
        ? (lesson ? lesson.title + ' · 复习' : '错词复习')
        : lesson.title,
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
                subject: 'chinese',
                item_count: l.words.length,
                total_ms: l.words.length * 4500 + 3200,
              })),
            });
            return;
          }
          if (path === '/api/review/summary' && method === 'GET') {
            resolve({
              due_count: mockDue,
              wrong_count: mockWrong,
              review_count: mockReviewCount(),
              subjects: mockReviewCount() > 0
                ? [{ subject: 'chinese', due_count: mockDue, wrong_count: mockWrong, review_count: mockReviewCount(), oldest_overdue_days: 1 }]
                : [],
            });
            return;
          }
          if (path === '/api/sessions' && method === 'POST') {
            resolve(buildMockManifest('lesson', body && body.lesson_id));
            return;
          }
          if (path === '/api/review/lessons' && method === 'GET') {
            const count = mockReviewCount();
            const lesson = MOCK_LESSONS.filter((l) => l.id === MOCK_REVIEW_LESSON_ID)[0];
            resolve({
              subject: null,
              lessons: count > 0 && lesson
                ? [{
                    lesson_id: lesson.id,
                    dir: lesson.title,
                    title: lesson.title,
                    review_count: count,
                    due_count: mockDue,
                    wrong_count: mockWrong,
                    learned_count: lesson.words.length,
                  }]
                : [],
            });
            return;
          }
          if (path === '/api/sessions/review' && method === 'POST') {
            const lessonId = (body && body.lesson_id) || null;
            if (mockReviewCount() <= 0) throw httpError(409, '这一课没有需要复习的词');
            if (lessonId && lessonId !== MOCK_REVIEW_LESSON_ID) {
              throw httpError(409, '这一课没有需要复习的词');
            }
            resolve(buildMockManifest('review', lessonId));
            return;
          }
          const mockJob = /^\/api\/jobs\/([^/]+)$/.exec(path);
          if (mockJob && method === 'GET') {
            resolve({ job_id: mockJob[1], status: 'done', result: { tts_calls: 1 } });
            return;
          }
          const regenerate = /^\/api\/lessons\/([^/]+)\/regenerate-word$/.exec(path);
          if (regenerate && method === 'POST') {
            if (!body || !body.text) throw httpError(400, '缺少要重新生成的词');
            resolve({ ok: true, text: body.text });
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

  /** 静态模式：只读 JSON（R2 / Pages），无鉴权、无写操作。 */
  async function fetchJson(url) {
    let res;
    try {
      res = await fetch(url, { cache: 'no-cache' });
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

  function newSessionId() {
    try {
      if (window.crypto && window.crypto.randomUUID) return window.crypto.randomUUID();
    } catch (err) { /* 老浏览器走 fallback */ }
    return 'st-' + Date.now().toString(36) + '-' + Math.random().toString(36).slice(2, 10);
  }

  /**
   * 发布的 lesson.json 缺 session_id / base，条目也不带 item.base；
   * 补成 SessionManifest 的形状，播放器无需感知差异。
   */
  function hydrateStaticManifest(lesson, lessonUrl) {
    const manifest = Object.assign({}, lesson);
    manifest.session_id = newSessionId();
    manifest.kind = manifest.kind || 'lesson';
    manifest.base = new URL('.', lessonUrl).href.replace(/\/+$/, '');
    return manifest;
  }

  const serverApi = {
    lessons: () => request('/api/lessons'),
    reviewSummary: () => request('/api/review/summary'),
    reviewLessons: () => request('/api/review/lessons'),
    createSession: (lessonId) => request('/api/sessions', { method: 'POST', body: { lesson_id: lessonId } }),
    createReview: (subject) => request('/api/sessions/review', { method: 'POST', body: { subject: subject } }),
    lessonReview: (subject, lessonId) =>
      request('/api/sessions/review', {
        method: 'POST',
        body: { subject: subject, lesson_id: lessonId },
      }),
    getSession: (sessionId) => request('/api/sessions/' + encodeURIComponent(sessionId)),
    check: (sessionId, results) =>
      request('/api/sessions/' + encodeURIComponent(sessionId) + '/check', {
        method: 'POST',
        body: { results: results },
      }),
    regenerateWord: (lessonId, text) =>
      request('/api/lessons/' + encodeURIComponent(lessonId) + '/regenerate-word', {
        method: 'POST',
        body: { text: text },
      }),
    job: (jobId) => request('/api/jobs/' + encodeURIComponent(jobId)),
    importResult: (payload) =>
      request('/api/results/import', { method: 'POST', body: payload }),
  };

  const staticApi = {
    async lessons() {
      let index;
      try {
        index = await fetchJson(dataUrl('lessons/index.json'));
      } catch (err) {
        // 新发布站还没有任何课时：索引 404 视为空列表，而不是报错
        if (err && err.status === 404) return { lessons: [] };
        throw err;
      }
      const lessons = Array.isArray(index && index.lessons) ? index.lessons : [];
      return { lessons: lessons };
    },
    // 没有复习概念：返回空让首页自然不渲染复习入口
    reviewSummary: () =>
      Promise.resolve({ due_count: 0, wrong_count: 0, review_count: 0, subjects: [] }),
    reviewLessons: () => Promise.resolve({ lessons: [] }),
    async createSession(lessonId) {
      const entry = (state.lessons || []).find((l) => String(l.id) === String(lessonId));
      const url = entry && entry.path
        ? dataUrl(entry.path)
        : dataUrl('lessons/' + encodeURIComponent(lessonId) + '/lesson.json');
      const lesson = await fetchJson(url);
      if (!lesson || typeof lesson !== 'object') {
        throw httpError(404, '找不到这一课');
      }
      if (Number(lesson.schema_version) > 1) {
        throw httpError(400, '这一课用了更新的格式，先刷新页面');
      }
      if (!Array.isArray(lesson.items) || !lesson.items.length) {
        throw httpError(400, '这一课还没有可播放的内容');
      }
      return hydrateStaticManifest(lesson, url);
    },
    // 本地判分：只统计对错，结果不入库（导出走结果页）
    check: (sessionId, results) => {
      const wrong = results.filter((r) => !r.correct).length;
      return Promise.resolve({ correct: results.length - wrong, wrong: wrong, due_count: 0 });
    },
    createReview: () => Promise.reject(httpError(400, '分享版不支持错词复习')),
    lessonReview: () => Promise.reject(httpError(400, '分享版不支持课内复习')),
    getSession: () => Promise.reject(httpError(404, '分享版没有会话记录')),
    regenerateWord: () => Promise.reject(httpError(400, '分享版不能重新生成音频')),
    job: () => Promise.reject(httpError(404, '分享版没有后台任务')),
    importResult: () => Promise.reject(httpError(400, '分享版不能导入结果')),
  };

  const API = STATIC ? staticApi : serverApi;

  /* --------------------------------------------------------------- 界面状态 */

  const SCREENS = ['home', 'player', 'done', 'check', 'result', 'error'];
  const LAST_LESSON_KEY = 'dictation.lastLesson';

  const state = {
    lessons: [],
    reviewCount: 0,
    reviewSubjects: [],
    deepLink: null,
    marked: new Set(),
    busy: false,
    submitting: false,
    regenerating: false,
    patching: false,
    importing: false,
    clipVersions: new Map(),
    lastExport: null,
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

  /* ------------------------------------------------------------- 隐藏：长按 */

  const HOLD_MS = 1500;
  const HOLD_MOVE_PX = 10;

  /**
   * 长按触发。只用 pointer 事件；滑动、抬起、取消都会中止计时。
   * 目标元素加 .hold-target（禁止系统长按菜单 / 选中文本）。
   */
  function attachLongPress(node, onHold) {
    let timer = null;
    let startX = 0;
    let startY = 0;
    let armed = false;

    // 同一个长按可能同时走到 pointer 计时和 contextmenu（微信/部分 WebView
    // 会把长按转成 contextmenu），800ms 内只触发一次。
    let lastFiredAt = 0;
    const fire = () => {
      const now = Date.now();
      if (now - lastFiredAt < 800) return;
      lastFiredAt = now;
      swallowNextClick();
      onHold();
    };

    // 长按抬手时浏览器还会补发一次 click：在捕获阶段吞掉它，
    // 否则刚弹出的面板/确认框会被这次 click 立刻点掉。
    // 只吞一次、且 400ms 内失效，避免挡住家长随后的真实点击。
    const swallowNextClick = () => {
      let done = false;
      const swallow = (event) => {
        if (done) return;
        done = true;
        document.removeEventListener('click', swallow, true);
        event.preventDefault();
        event.stopPropagation();
      };
      document.addEventListener('click', swallow, true);
      setTimeout(() => {
        if (done) return;
        done = true;
        document.removeEventListener('click', swallow, true);
      }, 400);
    };

    const cancel = () => {
      armed = false;
      if (timer) { clearTimeout(timer); timer = null; }
    };

    node.addEventListener('pointerdown', (event) => {
      if (event.pointerType === 'mouse' && event.button !== 0) return;
      cancel();
      armed = true;
      startX = event.clientX;
      startY = event.clientY;
      try { node.setPointerCapture(event.pointerId); } catch (e) { /* ignore */ }
      timer = setTimeout(() => {
        timer = null;
        armed = false;
        fire();
      }, HOLD_MS);
    });

    node.addEventListener('pointermove', (event) => {
      if (!armed) return;
      const dx = event.clientX - startX;
      const dy = event.clientY - startY;
      if (dx * dx + dy * dy > HOLD_MOVE_PX * HOLD_MOVE_PX) cancel();
    });

    node.addEventListener('pointerup', cancel);
    node.addEventListener('pointercancel', cancel);
    node.addEventListener('lostpointercapture', cancel);
    // 微信内置 WebView 等会把长按转成 contextmenu：拦下系统菜单并当作长按。
    node.addEventListener('contextmenu', (event) => {
      event.preventDefault();
      cancel();
      fire();
    });
  }

  /* ------------------------------------------- 隐藏：听写节奏（纯本地设置） */

  const TIMING_KEY = 'dictation.timing.v1';
  const TIMING_DEFAULT_SECONDS = 6;
  const TIMING_MAX_SECONDS = 120;
  // 片段自带 20ms 前导 + 60ms 尾音，可听静音 ≈ gap + 80ms
  const CLIP_TAIL_MS = 80;
  let timingOpenedAt = 0;

  function clampTimingSeconds(value) {
    if (value === null || value === undefined) return null;
    if (typeof value === 'string' && value.trim() === '') return null;
    const n = Number(value);
    if (!isFinite(n)) return null;
    if (n < 0 || n > TIMING_MAX_SECONDS) return null;
    return Math.round(n * 10) / 10;
  }

  /** 返回设置里的秒数；没有设置（或读不到）返回 null */
  function readTimingSeconds() {
    let raw = null;
    try { raw = localStorage.getItem(TIMING_KEY); } catch (e) { return null; }
    if (!raw) return null;
    let data = null;
    try { data = JSON.parse(raw); } catch (e) { data = null; }
    if (!data || typeof data !== 'object') return null;
    const seconds = clampTimingSeconds(data.gapSeconds);
    return seconds === null ? null : seconds;
  }

  /** 词后等待（毫秒）：设置了就用它，否则返回 null 让调用方走 manifest */
  function timingGapMs() {
    const seconds = readTimingSeconds();
    if (seconds === null) return null;
    return Math.max(0, Math.round(seconds * 1000) - CLIP_TAIL_MS);
  }

  function saveTimingSeconds(seconds) {
    try {
      localStorage.setItem(TIMING_KEY, JSON.stringify({ gapSeconds: seconds }));
      return true;
    } catch (e) {
      return false;
    }
  }

  function clearTimingSeconds() {
    try { localStorage.removeItem(TIMING_KEY); } catch (e) { /* ignore */ }
  }

  function openTimingPanel() {
    const stored = readTimingSeconds();
    els.timingGap.value = String(stored === null ? TIMING_DEFAULT_SECONDS : stored);
    timingOpenedAt = Date.now();
    els.timingModal.hidden = false;
    setTimeout(() => {
      try { els.timingGap.focus({ preventScroll: true }); } catch (e) { /* ignore */ }
      try { els.timingGap.select(); } catch (e) { /* ignore */ }
    }, 30);
  }

  function closeTimingPanel() {
    els.timingModal.hidden = true;
  }

  function submitTimingPanel() {
    const seconds = clampTimingSeconds(els.timingGap.value);
    if (seconds === null) {
      toast('填 0 到 120 之间的秒数');
      try { els.timingGap.focus({ preventScroll: true }); } catch (e) { /* ignore */ }
      return;
    }
    const saved = saveTimingSeconds(seconds);
    closeTimingPanel();
    toast(saved ? '已保存' : '这台设备存不了设置');
  }

  function resetTimingPanel() {
    clearTimingSeconds();
    closeTimingPanel();
    toast('已恢复默认节奏');
  }

  /* ------------------------------------------------ 导入结果（家长） */

  function openImportModal() {
    if (!CAP.importResult) return;
    closeTimingPanel();
    els.importFile.value = '';
    els.importText.value = '';
    els.importStatus.textContent = '';
    els.importModal.hidden = false;
  }

  function closeImportModal() {
    els.importModal.hidden = true;
  }

  function readFileText(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || ''));
      reader.onerror = () => reject(new Error('读取文件失败'));
      reader.readAsText(file);
    });
  }

  async function submitImport() {
    if (state.importing) return;
    const file = els.importFile.files && els.importFile.files[0];
    let text = els.importText.value.trim();
    if (file) {
      try {
        text = (await readFileText(file)).trim();
      } catch (e) {
        toast('读取文件失败，换一个文件试试');
        return;
      }
    }
    if (!text) {
      toast('先选文件，或粘贴结果 JSON');
      return;
    }

    let payload;
    try {
      payload = JSON.parse(text);
    } catch (e) {
      els.importStatus.textContent = '这不是有效的 JSON。';
      return;
    }

    state.importing = true;
    els.importSubmit.disabled = true;
    els.importStatus.textContent = '导入中…';
    try {
      const data = await API.importResult(payload);
      const duplicate = data && data.status === 'duplicate';
      els.importStatus.textContent = duplicate
        ? '这份结果之前已经导入过，没有重复计分。'
        : '已导入：' + data.correct + ' 对 / ' + data.wrong + ' 错'
          + (data.review_count > 0 ? '；待复习 ' + data.review_count + ' 个词' : '');
      toast(duplicate ? '已经导入过了' : '导入完成');
      await loadHome(); // 刷新首页的复习入口
    } catch (err) {
      els.importStatus.textContent = (err && err.message) || '导入失败，再试一次';
    } finally {
      state.importing = false;
      els.importSubmit.disabled = false;
    }
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
    const silent = wavDataUri(new Float32Array(160));
    try {
      audio.src = silent;
      const p = audio.play();
      if (p && p.then) {
        p.then(() => { try { audio.pause(); } catch (e) { /* ignore */ } })
          .catch(() => { /* 解锁失败就等首次 play 再试 */ });
      }
    } catch (e) { /* ignore */ }
    try {
      if (trackEl && trackEl !== audio) {
        trackEl.src = silent;
        const p = trackEl.play();
        if (p && p.then) {
          p.then(() => { try { trackEl.pause(); } catch (e) { /* ignore */ } })
            .catch(() => { /* 等整轨 play 时再试 */ });
        }
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
    const url = mediaUrl(base, clip.path, clip.duration_ms);
    if (!url || MOCK) return url;
    // 重新生成后片段是同名覆盖的：同一个 URL 浏览器可能复用旧音频，
    // 加一个每次重生成都会变的查询参数强制回源。
    const version = state.clipVersions.get(item.text);
    if (!version) return url;
    return url + (url.indexOf('?') === -1 ? '?' : '&') + 'v=' + version;
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
    startedAt: '',
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

    // 隐藏的「听写节奏」：只覆盖词后等待，不影响 repeat_gap / lead_in / intro_gap
    const customGap = timingGapMs();

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
      setStatus(player.paused ? '已暂停' : '正在听写');
      saveResume(m, i);
      if (!(await waitWhilePaused(runId))) return;

      const repeat = Math.max(1, Math.round(num(item.repeat, d.repeat, 1)));
      const repeatGap = num(item.repeat_gap_ms, d.repeat_gap_ms, 1200);
      const gap = customGap === null ? num(item.gap_ms, d.gap_ms, 0) : customGap;
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


  /* ------------------------------------------------------------ 整轨引擎（C1） */

  // 播放时把「片段 + 静音」在内存里拼成一条 WAV，交给单个 media 元素连续播放。
  // 节奏存在于音频样本里，不依赖 JS 定时器：熄屏 / 切 App 不会被挂起。
  // 契约与边界：docs/playback-track.md。拼装失败自动回退到分片引擎。

  const TRACK_BYTES_PER_MS = 48; // 24000 Hz × 16-bit × mono
  const TRACK_MAX_BYTES = 32 * 1024 * 1024;
  const trackEl = $('track');
  const track = {
    head: [],
    tail: null,
    chunks: [],
    starts: [],
    bytes: 0,
    durationMs: 0,
    url: null,
    ready: false,
    lastError: '',
  };

  // ?track=0 强制分片引擎（钉钉/微信里没有 devtools，保留一个逃生口）
  const trackFlag = new URLSearchParams(location.search).get('track');
  let trackEnabled = trackFlag !== '0';
  let trackBroken = false;

  function usingTrack() {
    return trackEnabled && track.ready && !trackBroken && !MOCK && !!trackEl;
  }

  function canAssembleTrack() {
    return trackEnabled && !trackBroken && !MOCK && !!trackEl
      && typeof Blob !== 'undefined' && typeof URL !== 'undefined'
      && typeof URL.createObjectURL === 'function';
  }

  function silenceBytes(ms) {
    const frames = Math.max(0, Math.round((Number(ms) || 0) * 24));
    return new Uint8Array(frames * 2);
  }

  function wavHeader(dataBytes) {
    const buf = new ArrayBuffer(44);
    const dv = new DataView(buf);
    const write = (off, str) => {
      for (let i = 0; i < str.length; i++) dv.setUint8(off + i, str.charCodeAt(i));
    };
    write(0, 'RIFF');
    dv.setUint32(4, 36 + dataBytes, true);
    write(8, 'WAVE');
    write(12, 'fmt ');
    dv.setUint32(16, 16, true);
    dv.setUint16(20, 1, true);      // PCM
    dv.setUint16(22, 1, true);      // mono
    dv.setUint32(24, 24000, true);
    dv.setUint32(28, 48000, true);  // byte rate
    dv.setUint16(32, 2, true);      // block align
    dv.setUint16(34, 16, true);     // bits
    write(36, 'data');
    dv.setUint32(40, dataBytes, true);
    return new Uint8Array(buf);
  }

  /** 按 chunk 遍历 RIFF，取 data；并校验 24k / mono / s16（不合规就回退）。 */
  function readWavPcm(buf) {
    const dv = new DataView(buf);
    if (buf.byteLength < 44 || dv.getUint32(0, false) !== 0x52494646) {
      throw new Error('不是 RIFF/WAV');
    }
    let off = 12;
    let fmt = null;
    let data = null;
    while (off + 8 <= buf.byteLength) {
      const id = dv.getUint32(off, false);
      const size = dv.getUint32(off + 4, true);
      if (id === 0x666d7420) {
        fmt = {
          format: dv.getUint16(off + 8, true),
          channels: dv.getUint16(off + 10, true),
          sampleRate: dv.getUint32(off + 12, true),
          bits: dv.getUint16(off + 22, true),
        };
      } else if (id === 0x64617461) {
        data = new Uint8Array(buf, off + 8, Math.min(size, buf.byteLength - off - 8));
        break;
      }
      off += 8 + size + (size & 1);
    }
    if (!data) throw new Error('WAV 没有 data 块');
    if (!fmt || fmt.format !== 1 || fmt.channels !== 1
      || fmt.sampleRate !== 24000 || fmt.bits !== 16) {
      throw new Error('片段格式不是 24k/mono/s16');
    }
    return data;
  }

  async function fetchClipPcm(item, runId) {
    const src = clipSrc(item);
    if (!src) throw new Error('缺少片段地址');
    const res = await fetch(src);
    if (!res.ok) throw httpError(res.status, '片段读取失败（' + res.status + '）');
    const buf = await res.arrayBuffer();
    if (!alive(runId)) throw new Error('cancelled');
    return readWavPcm(buf);
  }

  /** 单个 item 的 chunk：repeat 份片段 + 遍间静音 + 词后静音。 */
  function buildItemChunk(item, pcm, defaults, customGap) {
    const d = defaults || {};
    const repeat = Math.max(1, Math.round(num(item.repeat, d.repeat, 1)));
    const repeatGap = silenceBytes(num(item.repeat_gap_ms, d.repeat_gap_ms, 1200));
    // 隐藏节奏面板给的是「可听静音」，片段自带 80ms 尾音，这里复用同一换算
    const gapMs = customGap === null ? num(item.gap_ms, d.gap_ms, 0) : customGap;
    const gap = silenceBytes(gapMs);
    const chunk = [];
    for (let r = 0; r < repeat; r++) {
      chunk.push(pcm);
      if (r < repeat - 1) chunk.push(repeatGap);
    }
    chunk.push(gap);
    const bytes = pcm.length * repeat + repeatGap.length * (repeat - 1) + gap.length;
    return { chunk, bytes };
  }

  /** 拼整条整轨：总是从头拼（续听用 seek），返回 Blob URL 与每项起始偏移。 */
  async function assembleTrack(manifest, runId) {
    const d = manifest.defaults || {};
    const items = manifest.items || [];
    const customGap = timingGapMs();
    const head = [];
    let bytes = 0;

    const pushSilence = (bucket, ms) => {
      const silence = silenceBytes(ms);
      if (!silence.length) return;
      bucket.push(silence);
      bytes += silence.length;
    };

    pushSilence(head, num(d.lead_in_ms, 3000));
    if (manifest.intro) {
      const pcm = await fetchClipPcm({ clips: [manifest.intro], text: '__intro__' }, runId);
      head.push(pcm);
      bytes += pcm.length;
      pushSilence(head, num(d.intro_gap_ms, 1500));
    }

    const chunks = new Array(items.length);
    const starts = new Array(items.length);
    for (let i = 0; i < items.length; i++) {
      if (!alive(runId)) throw new Error('cancelled');
      starts[i] = bytes / TRACK_BYTES_PER_MS / 1000;
      const pcm = await fetchClipPcm(items[i], runId);
      const built = buildItemChunk(items[i], pcm, d, customGap);
      chunks[i] = built.chunk;
      bytes += built.bytes;
      if (bytes > TRACK_MAX_BYTES) throw new Error('整轨超过 32MB');
    }

    let tail = null;
    if (manifest.outro) {
      tail = await fetchClipPcm({ clips: [manifest.outro], text: '__outro__' }, runId);
      bytes += tail.length;
    }

    const parts = [wavHeader(bytes)];
    parts.push.apply(parts, head);
    chunks.forEach((chunk) => parts.push.apply(parts, chunk));
    if (tail) parts.push(tail);

    const blob = new Blob(parts, { type: 'audio/wav' });
    return {
      head: head,
      tail: tail,
      chunks: chunks,
      starts: starts,
      bytes: bytes,
      durationMs: bytes / TRACK_BYTES_PER_MS,
      url: URL.createObjectURL(blob),
    };
  }

  /** 锁屏 / 通知栏的媒体卡片标题（按键仍交给系统默认媒体控制）。 */
  function updateMediaSession() {
    if (!('mediaSession' in navigator) || typeof MediaMetadata === 'undefined') return;
    const manifest = player.manifest;
    if (!manifest) return;
    try {
      navigator.mediaSession.metadata = new MediaMetadata({
        title: manifest.title || '听写',
        artist: '听写',
        album: subjectLabel(manifest.subject) || '',
      });
      navigator.mediaSession.playbackState = player.paused ? 'paused' : 'playing';
    } catch (e) { /* 老内核不支持就跳过 */ }
  }

  function clearMediaSession() {
    if (!('mediaSession' in navigator)) return;
    try {
      navigator.mediaSession.metadata = null;
      navigator.mediaSession.playbackState = 'none';
    } catch (e) { /* ignore */ }
  }

  function revokeTrackUrl() {
    if (!track.url) return;
    try { URL.revokeObjectURL(track.url); } catch (e) { /* ignore */ }
    track.url = null;
  }

  function cleanupTrack() {
    track.ready = false;
    track.head = [];
    track.tail = null;
    track.chunks = [];
    track.starts = [];
    track.bytes = 0;
    track.durationMs = 0;
    try { trackEl.pause(); } catch (e) { /* ignore */ }
    try {
      trackEl.removeAttribute('src');
      trackEl.load();
    } catch (e) { /* ignore */ }
    clearMediaSession();
    revokeTrackUrl();
  }

  function applyTrack(built) {
    revokeTrackUrl();
    track.head = built.head;
    track.tail = built.tail;
    track.chunks = built.chunks;
    track.starts = built.starts;
    track.bytes = built.bytes;
    track.durationMs = built.durationMs;
    track.url = built.url;
    track.ready = true;
  }

  /** currentTime → item 下标（二分；starts 升序）。 */
  function trackIndexAt(seconds) {
    const starts = track.starts;
    if (!starts.length) return 0;
    let lo = 0;
    let hi = starts.length - 1;
    while (lo < hi) {
      const mid = Math.ceil((lo + hi) / 2);
      if (starts[mid] <= seconds) lo = mid;
      else hi = mid - 1;
    }
    return lo;
  }

  function waitTrackReady() {
    return new Promise((resolve) => {
      if (!trackEl || trackEl.readyState >= 2) return resolve();
      let done = false;
      const finish = () => {
        if (done) return;
        done = true;
        trackEl.removeEventListener('canplay', finish);
        trackEl.removeEventListener('loadedmetadata', finish);
        resolve();
      };
      trackEl.addEventListener('canplay', finish);
      trackEl.addEventListener('loadedmetadata', finish);
      setTimeout(finish, 4000); // 兜底：个别内核不给事件也继续尝试播放
    });
  }

  /** 跳到第 index 项；resumePlaying=false 时不改变播放/暂停意图。 */
  function seekTrack(index, resumePlaying) {
    const items = (player.manifest && player.manifest.items) || [];
    if (!items.length) return;
    const next = Math.max(0, Math.min(Math.floor(Number(index)), items.length - 1));
    const at = track.starts[next] || 0;
    try { trackEl.currentTime = at + 0.001; } catch (e) { /* 元数据未就绪时忽略 */ }
    player.index = next;
    player.item = items[next];
    renderProgress(next, items.length);
    saveResume(player.manifest, next);
    if (resumePlaying !== false && !player.paused) {
      trackEl.play().catch(() => { /* 保持暂停态 */ });
    }
  }

  async function startTrack(runId, startIndex) {
    setStatus('准备音频…');
    const built = await assembleTrack(player.manifest, runId);
    if (!alive(runId)) {
      try { URL.revokeObjectURL(built.url); } catch (e) { /* ignore */ }
      return;
    }
    applyTrack(built);
    trackEl.src = built.url;
    await waitTrackReady(runId);
    if (!alive(runId)) return;

    if (startIndex > 0) {
      seekTrack(startIndex, false);
    } else {
      // 从头播放：必须留在 0，否则会跳过 lead_in 与开场白
      const items = player.manifest.items || [];
      player.index = 0;
      player.item = items[0] || null;
      renderProgress(0, items.length);
      saveResume(player.manifest, 0);
      try { trackEl.currentTime = 0; } catch (e) { /* 元数据未就绪时忽略 */ }
    }
    setReplayEnabled(true);
    setStatus('正在听写');
    updateMediaSession();
    await trackEl.play();
  }

  /** 页面回到前台 / 定时校正：以 currentTime 为准（ended 在后台可能不派发）。 */
  function syncTrackState() {
    if (!usingTrack() || !player.running) return;
    const nearEnd = track.durationMs > 0
      && trackEl.currentTime >= track.durationMs / 1000 - 0.3;
    if (trackEl.ended || nearEnd) {
      finishPlayback();
      return;
    }
    const items = player.manifest.items || [];
    const index = trackIndexAt(trackEl.currentTime);
    if (index !== player.index) {
      player.index = index;
      player.item = items[index] || null;
      renderProgress(index, items.length);
      saveResume(player.manifest, index);
    }
  }

  /** 长按重掷后只替换一个 chunk，并平移后续偏移（见 playback-track.md §6.2）。 */
  async function patchTrackItem(index) {
    if (!usingTrack() || !player.manifest) return false;
    const items = player.manifest.items || [];
    const item = items[index];
    if (!item) return false;

    const wasPaused = player.paused;
    state.patching = true;
    try {
      const pcm = await fetchClipPcm(item, player.runId);
      const built = buildItemChunk(item, pcm, player.manifest.defaults || {}, timingGapMs());
      track.chunks[index] = built.chunk;

      let bytes = 0;
      track.head.forEach((b) => { bytes += b.length; });
      const starts = new Array(items.length);
      for (let i = 0; i < track.chunks.length; i++) {
        starts[i] = bytes / TRACK_BYTES_PER_MS / 1000;
        track.chunks[i].forEach((b) => { bytes += b.length; });
      }
      if (track.tail) bytes += track.tail.length;

      const parts = [wavHeader(bytes)];
      parts.push.apply(parts, track.head);
      track.chunks.forEach((chunk) => parts.push.apply(parts, chunk));
      if (track.tail) parts.push(track.tail);

      const url = URL.createObjectURL(new Blob(parts, { type: 'audio/wav' }));
      revokeTrackUrl();
      track.url = url;
      track.bytes = bytes;
      track.durationMs = bytes / TRACK_BYTES_PER_MS;
      track.starts = starts;

      trackEl.src = url;
      await waitTrackReady(player.runId);
      if (!player.running) return false;
      seekTrack(index, false);
      if (wasPaused) trackEl.pause();
      else trackEl.play().catch(() => { /* ignore */ });
      return true;
    } finally {
      state.patching = false;
    }
  }

  /**
   * 锁屏 / 通知栏的播放暂停由系统直接操作媒体元素，不经过 togglePause()；
   * 监听元素自身的 play/pause 把状态同步回 UI 与断点续听。
   * 自己调 togglePause 时先改 player.paused 再操作元素，因此这里会跳过。
   */
  function onTrackNativePlay() {
    if (!player.running || !player.paused) return;
    player.paused = false;
    player.segStart = performance.now();
    updatePauseUI();
    updateMediaSession();
  }

  function onTrackNativePause() {
    if (!player.running || player.paused) return;
    player.activeMs += performance.now() - player.segStart;
    player.paused = true;
    saveCurrentResume();
    updatePauseUI();
    updateMediaSession();
  }

  function bindTrackEvents() {
    if (!trackEl) return;
    trackEl.addEventListener('play', onTrackNativePlay);
    trackEl.addEventListener('pause', onTrackNativePause);
    trackEl.addEventListener('timeupdate', () => {
      if (!usingTrack() || !player.running || player.paused) return;
      if (state.regenerating || state.patching) return;
      syncTrackState();
    });
    trackEl.addEventListener('ended', () => {
      if (usingTrack() && player.running) finishPlayback();
    });
    trackEl.addEventListener('error', () => {
      if (!usingTrack() || !player.running) return;
      trackBroken = true;
      showAudioError();
    });
  }

  /* ------------------------------------------------------- 诊断覆盖层（?diag=1） */

  function setupDiag() {
    if (new URLSearchParams(location.search).get('diag') !== '1') return;
    const box = el('pre', { id: 'diag-panel' });
    // 固定在右上角、pointer-events:none：不能挡住底部的暂停/重听按钮。
    // 需要复制内容时截图即可（面板本身不接收点击）。
    box.style.cssText = 'position:fixed;top:8px;right:8px;max-width:62vw;max-height:38vh;'
      + 'overflow:hidden;background:rgba(0,0,0,.72);color:#fff;font:10px/1.4 ui-monospace,monospace;'
      + 'padding:6px 8px;border-radius:8px;z-index:9999;white-space:pre-wrap;'
      + 'pointer-events:none;-webkit-user-select:none;user-select:none';
    document.body.appendChild(box);

    let lastVisibility = document.visibilityState;
    const snap = (tag) => {
      const dur = Number.isFinite(trackEl.duration) ? Math.round(trackEl.duration * 1000) : -1;
      box.textContent = [
        '[' + tag + '] ' + new Date().toISOString(),
        'vis=' + document.visibilityState + ' (was ' + lastVisibility + ')',
        'ua=' + String(navigator.userAgent || '').slice(0, 90),
        'wakeLock=' + ('wakeLock' in navigator)
          + ' mediaSession=' + ('mediaSession' in navigator)
          + ' audioSession=' + ('audioSession' in navigator),
        'track=' + (trackEnabled ? 'on' : 'off')
          + (trackBroken ? ' BROKEN' : '') + ' ready=' + track.ready,
        'blobBytes=' + track.bytes + ' trackMs=' + Math.round(track.durationMs)
          + ' audioMs=' + dur,
        'lastError=' + (track.lastError || '-'),
        't=' + (trackEl.currentTime || 0).toFixed(2)
          + ' idx=' + player.index + ' paused=' + player.paused
          + ' regen=' + state.regenerating + ' patch=' + state.patching,
      ].join('\n');
    };

    snap('init');
    document.addEventListener('visibilitychange', () => {
      snap('vis');
      lastVisibility = document.visibilityState;
    });
    setInterval(() => { if (!document.hidden) snap('tick'); }, 2000);
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
    updateNavButtons(start, items.length);
  }

  function renderProgress(index, total) {
    els.counterNow.textContent = String(Math.min(index + 1, total));
    const segs = els.segments.children;
    for (let i = 0; i < segs.length; i++) {
      segs[i].className = 'seg' + (i < index ? ' done' : i === index ? ' now' : '');
    }
    updateNavButtons(index, total);
  }

  function updateNavButtons(index, total) {
    const i = Math.floor(Number(index) || 0);
    const count = Math.max(0, Math.floor(Number(total) || 0));
    els.btnPrev.disabled = i <= 0;
    els.btnNext.disabled = count === 0 || i >= count - 1;
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
    state.lastExport = null;
    player.runId += 1;
    player.running = true;
    player.paused = false;
    player.index = start;
    player.item = null;
    player.startedAt = new Date().toISOString();
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
    if (canAssembleTrack()) {
      startTrack(runId, start).catch((err) => {
        if (!alive(runId)) return;
        console.warn('整轨拼装失败，回退逐词播放', err);
        track.lastError = String((err && err.message) || err);
        cleanupTrack();
        trackBroken = true;
        toast('整轨播放不可用，改用逐词播放');
        runTimeline(runId, start).catch((fallbackErr) => {
          console.error(fallbackErr);
          showAudioError();
        });
      });
    } else {
      runTimeline(runId, start).catch((err) => {
        console.error(err);
        showAudioError();
      });
    }
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
    cleanupTrack();
    releaseWakeLock();
  }

  function finishPlayback() {
    if (!player.running) return;
    clearResume();
    if (!player.paused) player.activeMs += performance.now() - player.segStart;
    player.running = false;
    player.paused = false;
    player.item = null;
    cleanupTrack();
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
      if (usingTrack()) {
        try { trackEl.pause(); } catch (e) { /* ignore */ }
      } else {
        try { audio.pause(); } catch (e) { /* ignore */ }
      }
      saveCurrentResume();
    } else {
      player.paused = false;
      player.segStart = performance.now();
      if (usingTrack()) {
        trackEl.play().catch(() => { /* ignore */ });
      } else if (player.clipFinish) {
        const p = audio.play();
        if (p && typeof p.catch === 'function') p.catch(() => { /* ignore */ });
      }
    }
    updatePauseUI();
    updateMediaSession();
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
    if (usingTrack()) {
      seekTrack(player.index, true);
      setStatus(player.paused ? '已暂停' : '正在听写');
      return;
    }
    player.replayRequested = true;
    if (player.clipFinish) player.clipFinish('replay');
  }

  /**
   * 跳到指定项的第一遍：结束当前项的剩余重复和等待。
   * 暂停时跳过去仍保持暂停；同项则忽略（重听当前词请用「重听这个词」）。
   */
  function jumpTo(target) {
    if (!player.running || !player.manifest) return;
    const items = player.manifest.items || [];
    if (!items.length) return;

    let index = Math.floor(Number(target));
    if (!isFinite(index)) return;
    index = Math.max(0, Math.min(index, items.length - 1));
    if (index === player.index) return;

    if (usingTrack()) {
      const wasPaused = player.paused;
      seekTrack(index, false);
      if (wasPaused) {
        try { trackEl.pause(); } catch (e) { /* ignore */ }
        setStatus('已暂停');
      } else {
        setStatus('正在听写');
      }
      return;
    }

    const wasPaused = player.paused;
    const elapsed = player.activeMs + (wasPaused ? 0 : performance.now() - player.segStart);

    startPlayback(player.manifest, index);

    // startPlayback 会清零计时；这里接着累计，避免「用时」缩水
    player.activeMs = elapsed;
    player.segStart = performance.now();

    if (wasPaused) {
      player.paused = true;
      try { audio.pause(); } catch (e) { /* ignore */ }
      updatePauseUI();
    } else {
      setStatus('正在听写');
    }
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

  function subjectLabel(subject) {
    if (subject === 'chinese') return '语文';
    if (subject === 'english') return '英语';
    return '';
  }

  function reviewCard(count, subject, oldest) {
    const label = subjectLabel(subject);
    const title = label ? '错词复习 · ' + label : '错词复习';
    let meta = '共 ' + count + ' 个词';
    if (oldest > 0) meta += ' · 最久 ' + oldest + ' 天未复习';
    meta += '，再听一遍';
    const card = el('button', { class: 'card card-review', type: 'button' }, [
      el('span', { class: 'review-count', text: String(count) }),
      el('div', { class: 'card-main' }, [
        el('p', { class: 'card-title', text: title }),
        el('p', { class: 'card-meta', text: meta }),
      ]),
      el('span', { class: 'chev' }, [iconEl('chev')]),
    ]);
    card.addEventListener('click', () => startReview(card, subject));
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
      resumePlayback(data);
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

  /* ------------------------------------------- 进度校验（删除 / 重建课程） */

  /** 断点续听对应的课时 id：static 用 lesson_id，server 从 base 提取。 */
  function resumeLessonId(manifest) {
    if (!manifest) return null;
    if (STATIC) {
      const id = String(manifest.lesson_id || '');
      return id || null;
    }
    return lessonIdFromManifest(manifest);
  }

  function findLessonById(id) {
    return (state.lessons || []).find((l) => String(l.id) === String(id)) || null;
  }

  /**
   * 课时被删除 / 重建后，本地进度可能指向不存在的数据：
   * - 课时不存在 → 清除进度，卡片不渲染；
   * - static：重新发布会改变 content_hash → 清除进度（server 不需要：
   *   续播时按当前课时新建会话，天然用新词表）。
   * 只在课时列表加载成功时执行；离线时保留卡片。
   */
  function pruneStaleResume() {
    const resume = readResume();
    if (!resume) return;
    const lessonId = resumeLessonId(resume.manifest);
    if (!lessonId) return;
    const lesson = findLessonById(lessonId);
    if (!lesson) {
      clearResume();
      toast('原课时已删除，进度已清除');
      return;
    }
    if (STATIC) {
      const saved = String(resume.manifest.content_hash || '');
      const current = String(lesson.content_hash || '');
      if (saved && current && saved !== current) {
        clearResume();
        toast('课时已更新，进度已重置');
      }
    }
  }

  /**
   * 「继续上次听写」：不再信任 localStorage 里的 manifest（课时可能已删除或
   * 重建），server 模式按当前课时新建会话并按词序号跳转；static 模式
   * manifest 即当前发布的数据（hash 已校验），直接续播。
   */
  async function resumePlayback(resume) {
    const lessonId = resumeLessonId(resume.manifest);
    if (!STATIC && lessonId) {
      try {
        const manifest = await API.createSession(lessonId);
        const last = Math.max(0, manifest.items.length - 1);
        const index = Math.max(0, Math.min(resume.index, last));
        rememberLesson(lessonId);
        startPlayback(manifest, index);
      } catch (err) {
        toast((err && err.message) || '没法继续，再试一次');
      }
      return;
    }
    startPlayback(resume.manifest, resume.index);
  }

  function deepLinkCard(manifest) {
    const items = manifest.items || [];
    const subject = subjectLabel(manifest.subject);
    const meta = [];
    if (subject) meta.push(subject);
    meta.push((items.length || 0) + ' 个词');
    meta.push('点击开始');
    const card = el('button', { class: 'card', type: 'button' }, [
      el('div', { class: 'card-main' }, [
        el('p', { class: 'card-title', text: manifest.title || '复习' }),
        el('p', { class: 'card-meta', text: meta.join(' · ') }),
      ]),
      el('span', { class: 'chev' }, [iconEl('chev')]),
    ]);
    card.addEventListener('click', () => {
      state.deepLink = null;
      unlockAudio();
      startPlayback(manifest);
      try { history.replaceState({}, '', location.pathname); } catch (e) { /* ignore */ }
    });
    return card;
  }

  function lessonCard(lesson, isLast) {
    const count = Number(lesson.item_count || 0);
    const reviewCount = Math.max(0, Number(lesson.review_count) || 0);
    const title = lesson.title || '未命名课时';
    const meta = [];
    const subject = subjectLabel(lesson.subject);
    if (subject) meta.push(subject);
    if (isLast) meta.push('上次');
    meta.push(count + ' 个词');
    if (lesson.total_ms) meta.push('约 ' + approxText(lesson.total_ms));

    const card = el('div', { class: 'card card-lesson' + (reviewCount > 0 ? ' has-review' : '') });

    // 主区域：不能嵌在 <button> 里（旁边还有复习按钮），用 role=button 保留键盘可达性
    const open = el('div', {
      class: 'card-open',
      role: 'button',
      tabindex: '0',
      'aria-label': '开始听写：' + title,
    }, [
      el('div', { class: 'card-main' }, [
        el('p', { class: 'card-title', text: title }),
        el('p', { class: 'card-meta', text: meta.join(' · ') }),
      ]),
      el('span', { class: 'chev' }, [iconEl('chev')]),
    ]);

    open.addEventListener('click', () => startLesson(lesson, card));
    open.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ' || e.key === 'Spacebar') {
        e.preventDefault();
        startLesson(lesson, card);
      }
    });
    card.appendChild(open);

    if (CAP.lessonReview && reviewCount > 0) {
      const reviewBtn = el('button', {
        class: 'card-review-btn',
        type: 'button',
        'aria-label': '复习 ' + reviewCount + '：' + title,
      }, [
        iconEl('retry'),
        el('span', { class: 'card-review-label', text: '复习 ' + reviewCount }),
      ]);
      reviewBtn.addEventListener('click', () => startLessonReview(lesson, card));
      card.appendChild(reviewBtn);
    }

    return card;
  }

  function emptyState() {
    const text = STATIC
      ? '还没有发布的课时。'
      : '还没有课时，先用 <code>dictation gen</code> 生成一课，刷新后就出现在这里。';
    return el('div', { class: 'empty' }, [el('p', { html: text })]);
  }

  function homeError(err) {
    const retry = el('button', { class: 'btn btn-secondary btn-block', type: 'button' }, [
      iconEl('retry'),
      el('span', { text: '重试' }),
    ]);
    retry.addEventListener('click', () => loadHome());
    const hint = (err && err.message)
      || (STATIC ? '检查一下网络，或者这个链接已经失效。' : '服务可能没启动，或者网络断了。');
    return [
      el('div', { class: 'empty' }, [
        el('p', { html: '<strong>读不到课时列表。</strong>' }),
        el('p', { text: hint }),
      ]),
      retry,
    ];
  }

  function renderHome() {
    const frag = document.createDocumentFragment();
    const resume = readResume();

    if (state.deepLink) {
      // 课时被删除后，共享的会话深链就是"死课"：词能看、音频 404，不渲染
      const lessonId = resumeLessonId(state.deepLink);
      if (!lessonId || findLessonById(lessonId)) {
        frag.appendChild(deepLinkCard(state.deepLink));
      } else {
        state.deepLink = null;
        toast('这一课可能已下线');
      }
    }

    if (CAP.review) {
      state.reviewSubjects.forEach((entry) => {
        if (entry.count > 0) {
          frag.appendChild(reviewCard(entry.count, entry.subject, entry.oldest));
        }
      });
    }
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

  async function bootHome() {
    const params = new URLSearchParams(location.search);
    if (CAP.sessionLink) {
      const sessionId = params.get('s');
      if (sessionId) {
        try {
          state.deepLink = await API.getSession(sessionId);
        } catch (err) {
          toast('这一课可能已下线');
        }
      }
    } else if (params.get('l')) {
      // 分享链接 ?l=<lesson_id>：直接把这一课挂到首页顶部
      try {
        state.deepLink = await API.createSession(params.get('l'));
      } catch (err) {
        toast('这一课可能已下线');
      }
    }
    await loadHome();
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

    const [lessonsRes, summaryRes, lessonReviewRes] = await Promise.allSettled([
      API.lessons(),
      API.reviewSummary(),
      API.reviewLessons(),
    ]);

    if (lessonsRes.status === 'rejected') {
      const frag = document.createDocumentFragment();
      if (state.deepLink) frag.appendChild(deepLinkCard(state.deepLink));
      const resume = readResume();
      if (resume) frag.appendChild(resumeCard(resume));
      homeError(lessonsRes.reason).forEach((node) => frag.appendChild(node));
      setChildren(els.homeBody, frag);
      return;
    }
    state.lessons = (lessonsRes.value && lessonsRes.value.lessons) || [];
    pruneStaleResume();

    // 课内复习：按 lesson_id 把 review_count 挂到课时卡片上；接口失败就当没有复习词
    const reviewByLesson = new Map();
    if (lessonReviewRes.status === 'fulfilled' && lessonReviewRes.value) {
      const rows = Array.isArray(lessonReviewRes.value.lessons) ? lessonReviewRes.value.lessons : [];
      rows.forEach((row) => {
        if (!row || !row.lesson_id) return;
        const n = Math.max(0, Number(row.review_count) || 0);
        if (n > 0) reviewByLesson.set(String(row.lesson_id), n);
      });
    }
    state.lessons.forEach((lesson) => {
      lesson.review_count = reviewByLesson.get(String(lesson.id)) || 0;
    });
    const summary = summaryRes.status === 'fulfilled' && summaryRes.value ? summaryRes.value : null;
    // review_count = 未纠正错词 + 到期词（当天标错的词也能立刻复习）；老服务端只有 due_count
    const hasReviewCount = summary && summary.review_count !== null && summary.review_count !== undefined;
    const reviewTotal = summary ? (hasReviewCount ? summary.review_count : summary.due_count) : 0;
    state.reviewCount = Math.max(0, Number(reviewTotal) || 0);

    const grouped = summary && Array.isArray(summary.subjects) ? summary.subjects : [];
    state.reviewSubjects = grouped.length
      ? grouped.map((entry) => ({
          subject: entry.subject,
          count: Math.max(0, Number(entry.review_count) || 0),
          oldest: Math.max(0, Number(entry.oldest_overdue_days) || 0),
        }))
      : (state.reviewCount > 0 ? [{ subject: 'chinese', count: state.reviewCount, oldest: 0 }] : []);
    renderHome();
  }

  function goHome() {
    disarmGuard();
    stopPlayback();
    loadHome();
  }

  /* --------------------------------------------------------------- 进入播放 */

  /** 课时卡片现在是 div，禁用态用 class 表达（保护重复点击） */
  function setCardBusy(card, busy) {
    if (!card) return;
    card.classList.toggle('is-busy', !!busy);
    card.setAttribute('aria-busy', busy ? 'true' : 'false');
  }

  async function startLesson(lesson, card) {
    if (state.busy) return;
    state.busy = true;
    setCardBusy(card, true);
    unlockAudio();
    try {
      const manifest = await API.createSession(lesson.id);
      state.busy = false;
      setCardBusy(card, false);
      rememberLesson(lesson.id);
      startPlayback(manifest);
    } catch (err) {
      state.busy = false;
      setCardBusy(card, false);
      toast((err && err.message) || '没法开始，再试一次');
    }
  }

  /** 课内复习：只练这一课的待复习词，走和「听写」同一套播放器 */
  async function startLessonReview(lesson, card) {
    if (state.busy) return;
    state.busy = true;
    setCardBusy(card, true);
    unlockAudio();
    try {
      const manifest = await API.lessonReview(lesson.subject, lesson.id);
      state.busy = false;
      setCardBusy(card, false);
      startPlayback(manifest);
    } catch (err) {
      state.busy = false;
      setCardBusy(card, false);
      if (err && err.status === 409) {
        toast('这一课没有需要复习的词');
        loadHome();
        return;
      }
      toast((err && err.message) || '没法开始，再试一次');
    }
  }

  async function startReview(card, subject) {
    if (state.busy) return;
    state.busy = true;
    card.disabled = true;
    unlockAudio();
    try {
      const manifest = await API.createReview(subject);
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

    // 隐藏：长按词文本用当前音色重新合成（仅课时会话；复习 / 自定义会话静默忽略）
    const wordEl = el('p', { class: 'check-word', text: item.text });
    if (CAP.regenerate) {
      const lessonId = lessonIdFromManifest(player.manifest);
      if (lessonId) {
        wordEl.classList.add('hold-target');
        attachLongPress(wordEl, () => regenerateCheckWord(item, lessonId));
      }
    }

    // 课时 session 里自动追加的错词带 review=true，标一个小标签区分（不占序号、不影响按钮）
    const wordCell = el('div', { class: 'check-word-cell' }, [
      wordEl,
      item.review ? el('span', { class: 'tag-review', text: '复习' }) : null,
    ]);

    row.append(
      el('span', { class: 'check-no', text: String(item.index || index + 1) }),
      wordCell,
      el('div', { class: 'check-tools' }, [soundBtn, markBtn])
    );
    return row;
  }

  /** 课时会话的 lesson_id 在 manifest.base 里（/media/<lesson_id>） */
  function lessonIdFromManifest(manifest) {
    if (!manifest || manifest.kind !== 'lesson') return null;
    const base = String(manifest.base || '');
    if (base.indexOf('/media/') !== 0) return null;
    const id = base.slice('/media/'.length).replace(/\/+$/, '');
    return id ? id : null;
  }

  /** 触发重生成并轮询到任务结束；片段文件同名覆盖，重听即生效 */
  async function regenerateWordAndWait(lessonId, text) {
    const res = await API.regenerateWord(lessonId, text);
    const jobId = res && res.job_id;
    if (!jobId) return;
    // 45 个词的课时首次渲染可能要几十秒（缓存冷时），给足时间
    for (let i = 0; i < 225; i++) {
      const job = await API.job(jobId);
      if (job && job.status === 'done') return;
      if (job && job.status === 'error') {
        throw new Error(job.error || '重新生成失败');
      }
      await new Promise((resolve) => setTimeout(resolve, 200));
    }
    throw new Error('重新生成超时，稍后回到这一课再试');
  }

  async function regenerateDuringPlayback(item, lessonId) {
    if (state.regenerating || state.submitting) return;
    const ok = await confirmDialog({
      title: '重新生成「' + item.text + '」？',
      text: '用当前音色重新合成，约 1 秒；完成后重听这个词就是新版',
      confirmLabel: '重新生成',
      cancelLabel: '取消',
    });
    if (!ok) return;

    state.regenerating = true;
    toast('正在重新生成…');
    try {
      await regenerateWordAndWait(lessonId, item.text);
      state.clipVersions.set(item.text, Date.now());
      let patched = false;
      if (usingTrack() && player.running) {
        const index = (player.manifest.items || []).findIndex((it) => it.text === item.text);
        if (index >= 0) patched = await patchTrackItem(index).catch(() => false);
      }
      toast(patched
        ? '已重新生成，已跳回这个词'
        : '已重新生成，重听这个词即可（不满意可再长按一次）');
    } catch (err) {
      toast((err && err.message) || '重新生成失败，稍后再试');
      if (window.console) console.warn('regenerate failed', err);
    } finally {
      state.regenerating = false;
    }
  }

  async function regenerateCheckWord(item, lessonId) {
    if (state.regenerating || state.submitting) return;
    const ok = await confirmDialog({
      title: '重新生成「' + item.text + '」？',
      text: '用当前音色重新合成，约 1 秒',
      confirmLabel: '重新生成',
      cancelLabel: '取消',
    });
    if (!ok) return;

    state.regenerating = true;
    try {
      await regenerateWordAndWait(lessonId, item.text);
      state.clipVersions.set(item.text, Date.now());
      let patched = false;
      if (usingTrack() && player.running) {
        const index = (player.manifest.items || []).findIndex((it) => it.text === item.text);
        if (index >= 0) patched = await patchTrackItem(index).catch(() => false);
      }
      toast(patched
        ? '已重新生成，已跳回这个词'
        : '已重新生成，重听这个词即可（不满意可再长按一次）');
    } catch (err) {
      toast((err && err.message) || '重新生成失败，稍后再试');
      if (window.console) console.warn('regenerate failed', err);
    } finally {
      state.regenerating = false;
    }
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
      ? (STATIC ? '错词可以导出，回家导入后会加入复习。' : '错词已加入复习，下次会再听一遍。')
      : '一个都没错，写得很稳。';

    const chips = [];
    if (manifest && manifest.items) {
      manifest.items.forEach((item, index) => {
        if (state.marked.has(index)) chips.push(el('span', { class: 'chip', text: item.text }));
      });
    }
    setChildren(els.resultWords, chips);

    // 导出：仅静态模式（server 端结果已入库，无需带回）
    if (STATIC) {
      state.lastExport = buildResultPayload();
      els.btnExport.hidden = false;
    } else {
      state.lastExport = null;
      els.btnExport.hidden = true;
    }

    disarmGuard();
    showScreen('result');
  }

  /* ------------------------------------------------------- 结果导出（静态） */

  function deviceLabel() {
    const ua = navigator.userAgent || '';
    if (/iPad/.test(ua) || (/Macintosh/.test(ua) && 'ontouchend' in document)) return 'iPad';
    if (/iPhone/.test(ua)) return 'iPhone';
    if (/Android/.test(ua)) return 'Android';
    return '桌面浏览器';
  }

  /** docs/share-publish.md §9.1：dictation.result/v1，自包含，不依赖课时匹配。 */
  function buildResultPayload() {
    const manifest = player.manifest || {};
    const items = (manifest.items || []).map((item, index) => ({
      position: index + 1,
      text: item.text,
      lang: item.lang,
      review: !!item.review,
      correct: !state.marked.has(index),
    }));
    const wrong = items.filter((item) => !item.correct).length;
    return {
      schema: 'dictation.result/v1',
      lesson: {
        lesson_id: String(manifest.lesson_id || ''),
        content_hash: String(manifest.content_hash || ''),
        title: String(manifest.title || ''),
        subject: String(manifest.subject || ''),
        url: manifest.base ? manifest.base + '/lesson.json' : '',
      },
      session: {
        kind: manifest.kind || 'lesson',
        started_at: player.startedAt || '',
        finished_at: new Date().toISOString(),
        device: deviceLabel(),
        student: '',
      },
      items: items,
      summary: { total: items.length, correct: items.length - wrong, wrong: wrong },
    };
  }

  function exportFilename(payload) {
    const now = new Date();
    const ymd = now.getFullYear()
      + String(now.getMonth() + 1).padStart(2, '0')
      + String(now.getDate()).padStart(2, '0');
    const title = String((payload.lesson && payload.lesson.title) || '听写')
      .replace(/[\\/:*?"<>|\s]+/g, '-');
    return title + '-' + ymd + '.json';
  }

  function downloadBlob(blob, filename) {
    const url = URL.createObjectURL(blob);
    const a = el('a', { href: url, download: filename });
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 2000);
  }

  function showExportText(text) {
    const close = () => overlay.remove();
    const copy = async () => {
      let ok = false;
      try {
        if (navigator.clipboard && navigator.clipboard.writeText) {
          await navigator.clipboard.writeText(text);
          ok = true;
        }
      } catch (err) { /* 走 execCommand 兜底 */ }
      if (!ok) {
        ta.focus();
        ta.select();
        try { ok = document.execCommand('copy'); } catch (err) { ok = false; }
      }
      toast(ok ? '已复制，去粘贴给家长' : '复制失败，长按文本手动复制');
    };

    const overlay = el('div', { class: 'modal modal-top' }, [
      el('div', { class: 'modal-card' }, [
        el('h2', { class: 'modal-title', text: '复制这份结果' }),
        el('p', { class: 'modal-text', text: '全选复制后，通过聊天软件发给家长。' }),
        (function () {
          const ta = el('textarea', { class: 'export-text', readonly: 'readonly', 'aria-label': '听写结果 JSON' });
          ta.value = text;
          return ta;
        })(),
        el('div', { class: 'modal-actions' }, [
          el('button', { class: 'btn btn-primary btn-block', type: 'button', onclick: copy }, [el('span', { text: '复制' })]),
          el('button', { class: 'btn btn-ghost btn-block', type: 'button', onclick: close }, [el('span', { text: '关闭' })]),
        ]),
      ]),
    ]);
    overlay.addEventListener('click', (event) => {
      if (event.target === overlay) close();
    });
    document.body.appendChild(overlay);
    const ta = overlay.querySelector('textarea');
  }

  async function exportResult() {
    const payload = state.lastExport;
    if (!payload) return;
    const filename = exportFilename(payload);
    const blob = new Blob([JSON.stringify(payload, null, 1) + '\n'], { type: 'application/json' });
    const file = new File([blob], filename, { type: 'application/json' });

    // 1. 系统分享面板（iOS / Android）
    try {
      if (navigator.canShare && navigator.canShare({ files: [file] })) {
        await navigator.share({ files: [file], title: payload.lesson.title });
        return;
      }
    } catch (err) {
      if (err && err.name === 'AbortError') return; // 用户取消
      if (window.console) console.warn('share failed', err);
    }

    // 2. 微信内置浏览器等不支持下载 → 直接给文本
    if (/MicroMessenger/i.test(navigator.userAgent || '')) {
      showExportText(JSON.stringify(payload, null, 1) + '\n');
      return;
    }

    // 3. 常规下载
    try {
      downloadBlob(blob, filename);
      toast('已保存到下载');
    } catch (err) {
      showExportText(JSON.stringify(payload, null, 1) + '\n');
    }
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

  /**
   * Android 浏览器（含微信/手机自带浏览器）长按会在部分内核上无视
   * user-select: none 直接开始选字；selectstart 是唯一可靠的拦截点，
   * 而且不会影响滚动。输入框保留正常选择。
   */
  function preventTextSelection() {
    document.addEventListener(
      'selectstart',
      (event) => {
        const tag = event.target && event.target.tagName;
        if (tag === 'INPUT' || tag === 'TEXTAREA' || (event.target && event.target.isContentEditable)) {
          return;
        }
        event.preventDefault();
      },
      true,
    );
  }

  function bindEvents() {
    els.btnToggle.addEventListener('click', togglePause);
    els.btnReplay.addEventListener('click', requestReplay);
    // 听写界面直接重掷当前词（45 个词的课不用等到检查页）
    attachLongPress(els.btnReplay, () => {
      const lessonId = lessonIdFromManifest(player.manifest);
      const item = player.item;
      if (!lessonId || !item) return;
      regenerateDuringPlayback(item, lessonId);
    });
    els.btnPrev.addEventListener('click', () => jumpTo(player.index - 1));
    els.btnNext.addEventListener('click', () => jumpTo(player.index + 1));

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

    els.btnExport.addEventListener('click', exportResult);
    els.btnResultHome.addEventListener('click', goHome);
    els.btnErrorHome.addEventListener('click', goHome);

    els.modalConfirm.addEventListener('click', () => closeModal(true));
    els.modalCancel.addEventListener('click', () => closeModal(false));
    els.modal.addEventListener('click', (event) => {
      if (event.target === els.modal) closeModal(false);
    });

    // 隐藏入口：首页标题长按 1.5s
    preventTextSelection();
    attachLongPress(els.homeTitle, openTimingPanel);
    els.timingClose.addEventListener('click', closeTimingPanel);
    els.timingImport.addEventListener('click', openImportModal);
    els.importClose.addEventListener('click', closeImportModal);
    els.importSubmit.addEventListener('click', submitImport);
    els.importModal.addEventListener('click', (event) => {
      if (event.target === els.importModal) closeImportModal();
    });
    els.timingSave.addEventListener('click', submitTimingPanel);
    els.timingReset.addEventListener('click', resetTimingPanel);
    els.timingModal.addEventListener('click', (event) => {
      if (event.target !== els.timingModal) return;
      if (Date.now() - timingOpenedAt < 400) return;
      closeTimingPanel();
    });
    els.timingGap.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        submitTimingPanel();
      }
    });

    document.addEventListener('keydown', (event) => {
      if (event.key !== 'Escape') return;
      if (!els.timingModal.hidden) { closeTimingPanel(); return; }
      if (!els.modal.hidden) closeModal(false);
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
        // 整轨模式不主动暂停：熄屏后由系统继续播放
        if (player.running && !player.paused && !usingTrack()) togglePause();
      } else if (player.running && !player.paused) {
        if (usingTrack()) syncTrackState();
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
      homeTitle: $('home-title'),
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
      btnPrev: $('btn-prev'),
      btnNext: $('btn-next'),

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
      btnExport: $('btn-export'),
      btnResultHome: $('btn-result-home'),

      btnErrorHome: $('btn-error-home'),

      modal: $('modal'),
      modalTitle: $('modal-title'),
      modalText: $('modal-text'),
      modalConfirm: $('modal-confirm'),
      modalCancel: $('modal-cancel'),

      timingImport: $('timing-import'),
      timingModal: $('timing-modal'),
      timingGap: $('timing-gap'),
      timingClose: $('timing-close'),
      timingReset: $('timing-reset'),
      timingSave: $('timing-save'),

      importModal: $('import-modal'),
      importFile: $('import-file'),
      importText: $('import-text'),
      importStatus: $('import-status'),
      importSubmit: $('import-submit'),
      importClose: $('import-close'),

      toast: $('toast'),
    };

    bindTrackEvents();
    setupDiag();
    els['mock-badge'].hidden = !MOCK;
    els.timingImport.hidden = !CAP.importResult;
    els['home-date'].textContent = todayLabel();

    bindEvents();
    bootHome();
  }

  boot();
})();
