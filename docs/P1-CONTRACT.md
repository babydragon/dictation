# P1 播放器与自检 — 实现契约

> 本文件是 P1 的实现契约。`AGENTS.md` 仍是总纲；冲突时以本文件的具体接口为准。
> 最后更新：2026-10-20

## 范围（v1）

- 首页：课时列表 + `错词复习 (N)` 入口。
- 播放：按 manifest 播放 intro → 词条 → outro；暂停/继续、重听当前词、再听一遍。
- 完成页：`检查`、`再听一遍`、`返回首页`。
- 检查页：显示本次所有词；只标错，未标默认正确；每行可单听；提交后落库。
- 记录：SQLite 存 attempts 与 review_state。
- 错词复习：首页入口，按到期错词生成一个复习 session；v1 不自动混入下一课。
- 自动混入下一课：下一阶段，不在本契约内。
- 不做：拍照批改、答案纸打印、账号、多用户、离线 Service Worker。

## 文件归属

| 范围 | 文件 |
|---|---|
| 服务端 | `Cargo.toml`、`src/lib.rs`、`src/main.rs`、`src/db.rs`、`src/server/mod.rs` |
| 前端 | `web/index.html`、`web/style.css`、`web/app.js`、`web/manifest.webmanifest` |
| 契约/文档 | `docs/P1-CONTRACT.md`、`AGENTS.md`、`README.md` |

服务端与前端不要互相修改对方的文件。

## 服务端 CLI

```bash
dictation serve \
  --dir lessons \
  --web web \
  --data data/dictation.db \
  --host 0.0.0.0 \
  --port 8080
```

- `--dir`：课时根目录，扫描 `*/manifest.json`。
- `--web`：静态前端目录。
- `--data`：SQLite 文件路径；父目录自动创建。
- 默认值同上；`--host` 可设 `127.0.0.1`。

## 数据库 schema

时间统一用 Unix 秒（INTEGER）。

```sql
CREATE TABLE IF NOT EXISTS lessons (
  id TEXT PRIMARY KEY,
  dir TEXT NOT NULL,
  title TEXT NOT NULL,
  item_count INTEGER NOT NULL,
  total_ms INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,              -- lesson | review
  lesson_id TEXT,
  title TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS session_items (
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  text TEXT NOT NULL,
  lang TEXT NOT NULL,
  clip_lesson TEXT NOT NULL,       -- lesson id, 用于 /media/<id>/...
  clip_path TEXT NOT NULL,         -- clips/xx.wav
  duration_ms INTEGER NOT NULL,
  gap_ms INTEGER NOT NULL,
  repeat INTEGER NOT NULL,
  is_review INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, position)
);

CREATE TABLE IF NOT EXISTS words (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  text TEXT NOT NULL UNIQUE,
  lang TEXT NOT NULL,
  clip_lesson TEXT,
  clip_path TEXT,
  duration_ms INTEGER,
  gap_ms INTEGER,
  repeat INTEGER,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  word_id INTEGER NOT NULL,
  grade TEXT NOT NULL,             -- again | good
  source TEXT NOT NULL,            -- self
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS review_state (
  word_id INTEGER PRIMARY KEY,
  due_at INTEGER NOT NULL,
  interval_days INTEGER NOT NULL,
  lapses INTEGER NOT NULL DEFAULT 0,
  last_grade TEXT NOT NULL
);
```

- `words.text` 是去重键；同一词在不同课共用状态，但 clip 信息更新为最近一次 lesson session 的。
- `review_state` 只在检查提交后写入/更新。
- 复习 session 不写 `words`，只引用已有 clip。

## API

### `GET /api/lessons`

扫描 `--dir` 下所有含 `manifest.json` 的目录，upsert `lessons`，返回：

```json
{
  "lessons": [
    { "id": "a1b2c3d4e5f6", "title": "第5课", "item_count": 20, "total_ms": 181160 }
  ]
}
```

- `id` = `sha256(dir_name)` 前 12 位 hex。
- 按 `updated_at` 倒序或标题排序均可；稳定即可。

### `GET /api/review/summary`

```json
{ "due_count": 0, "wrong_count": 1, "review_count": 1 }
```

- `due_count`：`review_state.due_at <= now` 的词数。
- `wrong_count`：`last_grade='again'`（尚未复习纠正）的词数。
- `review_count`：`last_grade='again' OR due_at <= now` 的词数（去重）。
- 首页 `错词复习 (N)` 用 `review_count`；这样孩子当天刚标错的词也能立即复习。
  `review_count=0` 时隐藏或禁用。

### `POST /api/sessions`

```json
{ "lesson_id": "a1b2c3d4e5f6" }
```

- 读取对应 lesson 的 `manifest.json`。
- 创建 `sessions` + `session_items`；upsert `words`（clip 信息取本课）。
- **自动混入**：在本课 items 之后、outro 之前，追加最多 5 个
  `last_grade='again' OR due_at <= now` 的错词/到期词；跳过与本课重复的词；
  不额外加语音提示；这些 item 的 `review=true`，`base` 指向原课。
- 返回 SessionManifest（见下）。
- 如果 lesson 不存在：404。

### `POST /api/sessions/review`

无 body。取 `last_grade='again' OR due_at <= now` 的 `review_state JOIN words`，
最多 20 个，按 `due_at` 升序（刚标错的词当天也能复习）：

- 创建 `sessions.kind='review'`、`title='错词复习'`。
- `session_items.is_review=1`，clip 用 `words` 里保存的。
- 返回 SessionManifest，`intro`/`outro` 为 `null`，`defaults.lead_in_ms=3000`。
- 没有到期词：`409 {"error":"没有到期错词"}`。

### `POST /api/sessions/{id}/check`

```json
{
  "results": [
    { "position": 1, "correct": true },
    { "position": 2, "correct": false }
  ]
}
```

- 必须覆盖 session 的所有 position；缺项 400。
- session 不存在 404；已 finished 409。
- 对每项：
  - upsert `words`（若新词，clip 信息从 session_items 取）。
  - 写 `attempts`，`grade = correct ? "good" : "again"`，`source="self"`。
  - 更新 `review_state`：
    - 错：`interval_days=1`、`due_at=now+86400`、`lapses+=1`、`last_grade="again"`。
    - 对：`interval_days` 按 `1→2→4→7→15→30` 递增（无记录从 2 开始；已 30 保持 30）；
      `due_at=now+interval*86400`、`last_grade="good"`。
  - 同一 session 重复提交被 409 拦住。
- 更新 `sessions.finished_at`。
- 返回：

```json
{ "correct": 18, "wrong": 2, "due_count": 2 }
```

### `GET /media/{lesson_id}/{path}`

- 通过 `lessons.id` 找 `dir`。
- 只允许 `clips/` 下的文件；其他 404。
- 防目录穿越：canonicalize 后必须仍在 lesson 目录内。
- Content-Type 按扩展名；WAV 用 `audio/wav`。

### 静态前端

- `GET /` → `web/index.html`
- `GET /app.js`、`/style.css`、`/manifest.webmanifest` 等 → 对应文件。
- API 与 `/media` 优先于静态 fallback。

## SessionManifest

服务端返回给播放器的 JSON。课时示例：

```json
{
  "session_id": "5b0d...",
  "kind": "lesson",
  "title": "第5课",
  "base": "/media/a1b2c3d4e5f6",
  "defaults": {
    "gap_ms": 6000,
    "repeat": 2,
    "repeat_gap_ms": 1200,
    "lead_in_ms": 3000,
    "intro_gap_ms": 1500
  },
  "intro": { "path": "clips/intro-xxxx.wav", "duration_ms": 3300 },
  "intro_text": "请准备好纸和笔，听写马上开始。",
  "outro": { "path": "clips/outro-xxxx.wav", "duration_ms": 2600 },
  "outro_text": "听写结束，请检查一遍。",
  "items": [
    {
      "index": 1,
      "text": "摇晃",
      "lang": "zh",
      "base": "/media/a1b2c3d4e5f6",
      "clips": [{ "path": "clips/01-xxxx.wav", "duration_ms": 1220 }],
      "repeat": 2,
      "repeat_gap_ms": 1200,
      "gap_ms": 6000,
      "review": false
    }
  ],
  "total_ms": 57956
}
```

- 播放器 URL = `item.base + "/" + clip.path`；intro/outro 用顶层 `base`。
- 复习 session：`kind="review"`、`title="错词复习"`、`intro=null`、`outro=null`；
  item 的 `base` 可能不同（来自原课），`review=true`。
- `total_ms` 包含本课 + 自动混入的复习词；播放器仍以实际音频时长为准。
- 检查页对 `review=true` 的词显示一个「复习」小标签，不改变播放顺序。

## 播放器时间轴

```
sleep(lead_in)
[intro] -> sleep(intro_gap)
for item in items:
  for r in 0..repeat:
    play(item.clips[0])
    if r < repeat-1: sleep(item.repeat_gap_ms)
  sleep(item.gap_ms)
[outro]
done
```

- 进度在每进入一个 item 时更新为 `index / total`；intro/outro 不占进度。
- 播放中不显示词。
- 暂停：暂停当前 audio；若在 sleep 中，睡眠循环暂停计时；继续时恢复。
- 重听当前词：暂停主流程，单独播当前 item 的 clip 一次，结束后恢复；不改变进度。
- 再听一遍：从第一个 item 重新播放（同一 session_id，不重新创建 session）。
- 播放失败（404/解码失败）：显示“音频加载失败”，提供返回首页。

## 检查页

- 显示本次所有 item（含复习词），顺序与播放一致。
- 每行：序号、词、`🔊` 单听、`标错` 开关。
- 默认全部正确；只有点过的才算错。再点一次取消。
- 底部：`已标错 N 个` + `完成检查` + `跳过`。
- `完成检查` 提交所有 position（correct = !marked）；成功后显示 `X 对 / Y 错`，错词已加入复习。
- 跳过：不写 attempts，返回首页。
- 提交失败：保留当前标记，提示重试；不要静默丢失。

## 首页

- 顶部：`听写`。
- 如果 `due_count > 0`：显示醒目的 `错词复习 (N)` 卡片，点击 POST `/api/sessions/review` 并进入播放器。
- 课时卡片：标题、词数、预计时长；点击 POST `/api/sessions` 并进入播放器。
- 空状态：`还没有课时，先用 dictation gen 生成一课`。
- 加载失败：显示重试按钮。

## 前端实现约定

- 纯静态 HTML/CSS/JS，无构建步骤；由 axum 直接托管。
- 不引入外部 CDN/字体；离线局域网可用。
- 手机优先；按钮最小 56px 高；主要按钮更大。
- 文本用中文；字号足够大，避免小字。
- 不显示答案直到完成页；检查页才显示词。
- 使用 `fetch` 调 API；同源，无 CORS。
- `sessionStorage`/`localStorage` 可选：记住首页最近课时；不做跨设备同步。
- 防误触：播放页“返回”需二次确认；检查页未提交离开需确认。
- 不自动播放音频；必须由用户点击“开始”后启动（iOS 限制）。

## 测试

服务端：
- 临时目录 + 临时 DB，生成一个 mock lesson，扫描、创建 session、提交 check、
  验证 `review_state` 到期时间、创建 review session。
- 路径穿越测试：`../` 404。
- 重复 check 409。

前端：
- 用真实服务端手动走一遍：首页 → 播放 → 完成 → 检查 → 提交 → 首页复习入口。
- 至少用 curl 验证 API JSON 与状态码。
