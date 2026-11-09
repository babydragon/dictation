# 服务端 HTTP API 契约

> 本文件定义 `serve` 的 HTTP 接口、SQLite schema、鉴权，以及「CLI 与 HTTP 一致性」
> 约定。`AGENTS.md` 是设计总纲；儿童端播放器与自检的交互见 [`player.md`](player.md)。
> 外网分享发布见本地工作稿 `docs/share-publish.md`（未入库，不随仓库发布）。
> 最后更新：2026-11-09

## 文件归属

| 范围 | 文件 |
|---|---|
| 服务端 | `Cargo.toml`、`src/lib.rs`、`src/main.rs`、`src/db.rs`、`src/server/mod.rs`、`src/lesson/import.rs`、`src/lesson/list.rs`、`src/result_view.rs` |
| 前端 | `web/index.html`、`web/style.css`、`web/app.js`、`web/manifest.webmanifest` |
| 契约/文档 | `docs/http-api.md`、`docs/player.md`、`AGENTS.md`、`README.md` |

服务端与前端不要互相修改对方的文件。

服务端写权限：`serve` 除读 `lessons/` 外，还会**写** `lessons/<课时>/`（导入与
生成）和 `cache/`；`data/` 由服务独占。systemd `ReadWritePaths` 必须等于
`config.toml` 的 `paths.base`。

## CLI 与 HTTP 的一致性约定

同一项能力如果 CLI 与 HTTP 都有入口，**必须是同一份实现**，不能各写一遍。

- 共用实现放在库里（`lesson::import`、`lesson::list`、`result_view`、`render`、
  `db`），CLI 与 `serve` 只做参数解析和输出格式。
- **新增或修改任何运维类操作时，两侧必须同步**：`lesson import/list/plan/gen`、
  `result list/show/wrong`、`config show`、`clean`，以后新增的 publish 等同样适用。
  只改一侧、或只改一侧的字段，视为未完成。
- **语义必须一致**：字段名、字段值、单位、排序、过滤条件、幂等行为完全相同。
  错误语义按载体映射，但含义一致：
  - HTTP：状态码 + `{"error":"中文单行原因"}`；
  - CLI：`exit 1` + stderr `{"error":"..."}`（`--json` 时）。
- 允许的差异只有**运行时能力**，且必须在本文写明理由：
  - HTTP 独有：job 队列/并发/取消、鉴权、`/media`、session 与 check；
  - CLI 独有：`clean`（破坏性，需人工确认）、`plan`（不落盘）、`new`、`config show`
    以及路径模式 `gen <lesson.txt> --out`（运维/调试）。
- 新增字段是兼容变更，可以只加不改；**删除或改名必须同时更新**契约、skill、
  README 和 PWA 适配层。
- 一致性检查清单（改完自查）：
  1. `lesson::list` 这类共用函数是否被两侧调用？
  2. 两侧 JSON 字段集合与排序是否相同？
  3. 错误分支（404/409/400）在 CLI 是否有等价提示？
  4. skill/README 里的示例是否需要同步？

## 服务端启动参数

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
    {
      "id": "a1b2c3d4e5f6",
      "dir": "第5课",
      "title": "第5课 词语听写",
      "item_count": 20,
      "total_ms": 181160
    }
  ]
}
```

- `id` = `sha256(dir_name)` 前 12 位 hex。
- `dir` 是**相对目录名**，不含主机绝对路径；与 `POST /api/lessons` 响应一致。
- 排序：`title` 升序，同名按 `dir` 升序（稳定、与 CLI 相同）。
- 没有课时：`200 {"lessons":[]}`，不是 404。
- 缺 `manifest.json` 的目录、manifest 解析失败的目录：跳过并 warn，不影响其他课时。
- **与 `dictation lesson list --json` 共用 `lesson::list::scan` + `lesson::list::json`**：
  字段集合、字段值、排序完全一致；CLI 的 `--json` 不再单独拼装。

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
- 返回 `SessionManifest`（字段与示例见 [`player.md`](player.md#sessionmanifest)）。
- 如果 lesson 不存在：404。

### `POST /api/sessions/review`

无 body。取 `last_grade='again' OR due_at <= now` 的 `review_state JOIN words`，
最多 20 个，按 `due_at` 升序（刚标错的词当天也能复习）：

- 创建 `sessions.kind='review'`、`title='错词复习'`。
- `session_items.is_review=1`，clip 用 `words` 里保存的。
- 返回 `SessionManifest`，`intro`/`outro` 为 `null`，`defaults.lead_in_ms=3000`。
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

## 服务端生成（agent 走 HTTP）

agent 不持有 TTS 密钥、也不需要文件系统权限：导入与合成全部走 HTTP，
密钥只存在于 `serve` 进程。

### 鉴权

- `DICTATION_API_TOKEN` **未设置**：写操作开放（家庭局域网默认）。
- **设置后**：下列端点要求请求头 `X-Dictation-Token: <token>`；缺失或不符返回
  `401 {"error":"未授权"}`。`GET` 端点不需要。
- 比较必须等长且常量时间；不要把 token 写进日志。
- 鉴权先于请求体解析：缺少/错误 token 时，即使 body 非法或 Content-Type 不对，也返回 401。
- 受保护端点：
  - `POST /api/lessons`
  - `POST /api/lessons/{id}/generate`
  - `POST /api/lessons/{id}/generate/cancel`

### `POST /api/lessons`

body 与 CLI `lesson import --payload` 的 JSON 相同：

```json
{
  "title": "第5课 词语听写",
  "dir": "第5课",
  "text": "摇晃\n粗壮\n毕竟\n",
  "toml": "rate = 0.9\ngap = \"6s\"\n",
  "force": false
}
```

- 校验规则与 CLI 完全一致（共用 `lesson::import`）：`title` 必填、`text` 非空、
  单条 ≤200 字符、重复词保留首次并 warn、`dir` 生成安全目录名。
- 成功：`201`（新建）/ `200`（`force=true` 覆盖）

```json
{ "created": true, "dir": "第5课", "title": "第5课 词语听写", "item_count": 20 }
```

- `dir` 是**相对目录名**，不返回主机绝对路径。
- 已存在且 `force=false` → `409 {"error":"课时已存在","dir":"第5课"}`。
- 校验失败 → `400 {"error":"..."}`。

### `POST /api/lessons/{id}/generate`

`{id}` 解析顺序：先按 `lessons/` 下的**目录名**精确匹配；匹配不到再按 12 位
`lessons.id` 查 DB。两种都匹配不到 → `404`。

body 全部可选：

```json
{ "force": false, "full_track": true, "provider": "doubao" }
```

空 body 视为默认值；非空 body 一律按 JSON 解析，忽略请求声明的 Content-Type。
解析失败返回 `400 {"error":"请求体无效：..."}`。

- `provider` 省略时用 `DICTATION_PROVIDER`（默认 `qwen`）。
- `force=false` 且 `manifest.json` 比 `lesson.txt`/`lesson.toml` 新 →
  不重新合成，直接：

```json
{ "job_id": null, "status": "done", "skipped": true, "lesson": "第5课" }
```

- 已存在 `queued`/`running` 的同课任务 → 返回同一个 `job_id`，不重复计费。
- 否则入队，`202`：

```json
{ "job_id": "6f1c...", "status": "queued", "lesson": "第5课" }
```

- `503 {"error":"未配置 TTS 密钥（/etc/dictation/env）"}`：`serve` 启动时
  `Config::from_env()` 拿不到 key。
- `429 {"error":"生成队列已满"}`：排队上限 4。

### `GET /api/jobs/{job_id}`

```json
{
  "job_id": "6f1c...",
  "status": "running",
  "lesson": "第5课",
  "queued_at": "2026-11-06T18:00:00+08:00",
  "started_at": "2026-11-06T18:00:01+08:00",
  "finished_at": null,
  "result": null,
  "error": null
}
```

- `status`：`queued | running | done | error | cancelled`。
- `done` 时 `result`：

```json
{ "total_ms": 181160, "tts_calls": 20, "cache_hits": 3, "clips": 23 }
```

- `error` 时 `error` 是中文单行原因；不返回堆栈。
- 未知或已过期 job → `404`。
- job 仅在内存：完成后保留 1 小时，最多 100 条；服务重启后丢失（重触发即可，
  `cache/` 与 `manifest.json` 已落盘，不会重复计费）。

### `POST /api/lessons/{id}/generate/cancel`

- 只对 `queued` 生效，返回 `{"status":"cancelled"}`。
- `running` → `409 {"error":"任务已在执行，无法取消"}`。
- 未知 job/课时 → `404`。

### 并发与资源

- 全局同时只跑 **1** 个生成任务（SBC 双核）；其余排队。
- 生成在后台 task 中执行，不阻塞其他 API。
- 完成后无需通知：下次 `GET /api/lessons` 即可看到新课。

## 结果查询

沙箱里的 agent 读不到 SQLite，`result list/show/wrong` 只能主机侧跑。结果查询把
这三个**只读视图**原样搬到 HTTP：JSON 结构与 `--json` 完全相同，agent 不需要
第二套格式。

### 端点

| 方法 | 路径 | 等价 CLI |
|---|---|---|
| GET | `/api/results?limit=20&all=false` | `result list [--all]` |
| GET | `/api/results/{id}` | `result show <id>` |
| GET | `/api/results/latest` | `result show --latest` |
| GET | `/api/results/{id}/wrong` | `result wrong <id>` |
| GET | `/api/results/latest/wrong` | `result wrong --latest` |

- 只读，不写 `data/`；不触发扫描、不改 `lessons/`。
- **GET 不需要鉴权**（`DICTATION_API_TOKEN` 只保护写操作）。
- 不做 offset/游标分页：家庭规模下 `limit` 足够。
- `{id}` 是完整 session id；**不支持 8 位短 ID**（避免歧义），agent 先从列表取全 ID。
- `latest` 是静态段，优先于 `{id}` 匹配；它是「最近一次**已提交检查**的 session」，
  没有时返回 404 而不是回落到未完成的 session。

### 查询参数（仅 `/api/results`）

| 参数 | 默认 | 规则 |
|---|---|---|
| `limit` | `20` | 1–100 的整数；越界、非数字、空值 → 400 |
| `all` | `false` | `true`/`1` 含未提交检查的 session；`false`/`0` 只返回已提交 |

非法参数 → `400 {"error":"..."}`，**不静默夹取**（agent 的 bug 应当可见）。

### 响应

`GET /api/results`：

```json
{
  "sessions": [
    { "id": "5b0d...", "kind": "lesson", "title": "第5课",
      "created_at": "2026-11-06T10:00:00+08:00",
      "finished_at": "2026-11-06T10:05:12+08:00",
      "item_count": 20, "correct": 19, "wrong": 1 }
  ]
}
```

- `finished_at` 为 `null` 表示未提交检查（仅在 `all=true` 时可能出现）。
- 排序：完成时间倒序，未完成的按创建时间；稳定即可。
- 没有记录 → `200 {"sessions":[]}`，不是 404。

`GET /api/results/{id}`（detail）：

```json
{
  "session": { "...": "同上" },
  "items": [
    { "position": 1, "text": "摇晃", "lang": "zh", "review": false,
      "origin_lesson": "第5课", "grade": "good", "correct": true, "source": "self" }
  ],
  "wrong_words": [
    { "position": 2, "text": "粗壮", "lang": "zh", "review": false,
      "origin_lesson": "第5课", "grade": "again", "source": "self",
      "lapses": 1, "interval_days": 1,
      "due_at": "2026-11-07T10:05:12+08:00" }
  ]
}
```

`GET /api/results/{id}/wrong`：

```json
{ "session": { "...": "同上" }, "wrong_words": [ "...": "同上" ] }
```

- 未提交检查的 session，`items[].grade` / `source` 为 `null`，`correct` 为 `null`。
- `origin_lesson` 为 `null` 表示来源课已从 `lessons/` 消失；前端照常显示词。
- `due_at` 为 `null` 表示该词没有 `review_state`。
- 时间一律 RFC3339、`+08:00`，与 CLI `--json` 完全一致。

### 错误

| 情况 | 状态 | body |
|---|---|---|
| session 不存在 | 404 | `{"error":"session 不存在"}` |
| 没有已提交检查的记录（`latest`） | 404 | `{"error":"还没有已提交检查的听写记录"}` |
| `/wrong` 指向未提交检查的 session | 409 | `{"error":"本次听写还没提交检查"}` |
| `limit` 非法 | 400 | `{"error":"limit 必须是 1–100 的整数"}` |
| `all` 非法 | 400 | `{"error":"all 只能是 true/false/1/0"}` |

错误体不含绝对路径、不含堆栈。

### 实现约定

- `src/result_view.rs` 提供 `format_unix` / `session_json` / `item_json` / `wrong_json`。
  `main.rs` 的 `result` 子命令与 HTTP 端点都调用它，**禁止复制第二份序列化**。
- 处理器在 `spawn_blocking` 里查 DB（与其它 DB 端点一致），不阻塞运行时。
- `latest` 先 `Db::latest_finished_session()` 再取详情，语义与 CLI 一致。

## 测试

服务端：
- 临时目录 + 临时 DB，生成一个 mock lesson，扫描、创建 session、提交 check、
  验证 `review_state` 到期时间、创建 review session。
- 路径穿越测试：`../` 404。
- 重复 check 409。

CLI/HTTP 一致性：
- `GET /api/lessons` 与 `lesson::list::scan` 共用读取与序列化：响应同时含 `id`
  与 `dir`，排序为 `title` → `dir`；与 `dictation lesson list --json` 结构相同。
- 空目录返回 `200 {"lessons":[]}`；缺 manifest / manifest 损坏的目录被跳过，
  不影响其他课时。
- `GET /api/results...` 与 `result show/wrong --json` 逐字段相同（共用
  `result_view`，测试断言序列化函数而不是文本）。

服务端生成：
- `POST /api/lessons`：新建 201、重复 409、`force=true` 200、非法 payload 400；
  响应里不含绝对路径。
- `POST /api/lessons/{id}/generate`：mock provider 下 `202` → 轮询到 `done`，
  `manifest.json` 与 `clips/` 落盘；再触发一次 `skipped=true`；
- 同课并发触发两次返回同一 `job_id`。
- 未配置 key（`Config::from_env` 缺 key）→ `503`。
- `DICTATION_API_TOKEN` 设置后：无 header 401、错误 token 401、正确 token 放行；
  `GET` 不受影响。
- `cancel`：排队可取消，running 409。
- 导入校验与 CLI 共用，同一份 payload 两条路径结果一致。

结果查询：
- 空库：`GET /api/results` → 200 空列表；`/api/results/latest` → 404。
- 提交一次检查后：`list` / `{id}` / `{id}/wrong` / `latest` / `latest/wrong`
  字段与值正确；`finished_at`、`due_at` 是 RFC3339；`correct` 是布尔。
- `all=true` 才看得到未提交检查的 session；默认看不到。
- 未知 id → 404；未提交 session 的 `/wrong` → 409。
- `limit=0` / `limit=101` / `limit=abc` / `all=maybe` → 400。
- 设置 `DICTATION_API_TOKEN` 后，上述 GET 仍返回 200（GET 不受保护）。
