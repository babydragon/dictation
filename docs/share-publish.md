# 外网分享发布（Cloudflare Pages + R2）— 设计契约

> 状态：设计已确认，未实现。
> 前置阅读：[`AGENTS.md`](../AGENTS.md)（D13/D13b 的 HTTP 与密钥归属约定）、
> [`http-api.md`](http-api.md)（SessionManifest、鉴权与结果查询）。
>
> 本文件只写**契约与流程**。文件路径、目录权限、部署细节在实施时确认后再补到
> `INSTALL.md`；`AGENTS.md` 只在设计变化时同步。

---

## 1. 目标与非目标

**目标**：把已生成的课时发布到 Cloudflare，让家长/孩子在非家庭网络（旅途中）
用手机或平板直接听写；结果可导出，回家后导入，形成复习闭环。

**非目标（v1）**：

- 不做多租户、账号体系、按人计费。
- 不做在线编辑课时（家长端仍在本地）。
- 不做音频转码（沿用 24 kHz / 16-bit / 单声道 WAV）。
- 不做 R2 上的写接口（浏览器只读）。

**明确的边界**：听写内容不含隐私信息，**v1 不加访问控制**。以后要加，只需一条
WAF 规则 + 前端拼 URL，不改架构（见 §11）。

---

## 2. 架构：两条独立通道

```
① 壳（一次性 / 改前端时）
   web/  ──dictation publish --shell──▶  Pages 项目（dictation.example.com）
                                          public/            静态资源
                                          functions/data/*   R2 代理（私有桶）
                                          wrangler.jsonc     R2 binding

② 数据（每课一次，与 ① 无关）
   lessons/<id>/manifest.json + clips/
        └──dictation publish <课时>──▶  R2 桶（私有，无公网域名）
                                         lessons/index.json
                                         lessons/<id>/lesson.json
                                         lessons/<id>/clips/*.wav
```

**B 方案（已采用）**：R2 桶不暴露公网域名，Pages Function 通过 binding
读桶、以同源 `/data/*` 提供数据。没有 CORS，也没有第二个可寻址域名。

- 两条通道互不触发。发布新课时**不重新部署 Pages**。
- 静态模式下前端只有一套代码，通过 `config.js` 的 `mode` 切换数据源（§8）。
- **R2 是唯一真相源**。本地只是“本次要发布的这一课”的输入；本地历史丢失
  不影响 CF 上的存量。

---

## 3. 发布物布局

### 3.1 Pages（壳 + R2 代理）

`dictation publish --shell` 生成一个 Pages 工程目录（默认 `dist/pages`）：

```
dist/pages/
├── wrangler.jsonc              # name / pages_build_output_dir / R2 binding
├── functions/
│   └── data/[[path]].js        # /data/* -> 私有桶 lessons/*
└── public/                     # 静态壳
    ├── index.html
    ├── style.css
    ├── app.js
    ├── manifest.webmanifest
    ├── config.js               # { mode:'static', dataBase:'https://<pages>/data' }
    └── _headers
```

部署：`cd dist/pages && npx wrangler pages deploy`。

- 壳与数据仍然分离：发布课时只写 R2，**不重新部署 Pages**。
- 只有改前端 / 改 `config.js` / 改 Function 或 binding 时才需要重新部署。
- `dataBase` 默认取 `pages_base + /data`；也可在 `[publish].data_base` 显式指定。
- `--shell` 会给 `index.html` 里的 `style.css` / `config.js` / `app.js` 追加
  内容哈希查询串（`?v=<sha256前10位>`）。Pages 对静态资源有 4 小时默认缓存，
  且 `_headers` 的 `Cache-Control` 会被其覆盖，哈希 URL 才能保证重新部署后
  立即生效。

### 3.2 R2（数据）

```
<dataBase>/
├── lessons/index.json
└── lessons/<lesson_id>/
    ├── lesson.json
    ├── clips/01-152a8adf.wav
    ├── clips/02-3bddaf07.wav
    └── ...                      # 可选：full.mp3（--with-full）
```

- `<lesson_id>` = `db::lesson_id(dir_name)`（`sha256(dir_name)` 前 12 位十六进制）。
  **与内容无关，重新生成同一目录仍是同一个 id**，因此重发只会覆盖同一前缀。
- **发布时**按片段字节重新命名：`clips/<sha256(字节)前16位>.<ext>`。
  本地 `manifest.json` 里的名字不变，`lesson.json` 的 `clips[].path` 改写成发布名。
  这样同一 key 的字节永不变化（`immutable` 缓存安全），重新生成音频会产生新 URL；
  相同字节（含课内重复）只存一份。
- 首次从旧的「按词命名」切到内容寻址时，会整课重传一次；此后只上传变化的片段。
- 旧名字的对象保留（只增不删）：正在听旧 `lesson.json` 的客户端不会 404。
- **不上传 `full.wav`**（约 9.8 MB/课）。`full.mp3` 仅在 `--with-full` 时上传。
- 禁止写 `lessons/` 之外的 key。

---

## 4. 数据格式

### 4.1 `lessons/index.json`

```json
{
  "schema_version": 1,
  "updated_at": "2026-11-06T18:00:00+08:00",
  "lessons": [
    {
      "id": "a1b2c3d4e5f6",
      "title": "第5课",
      "subject": "chinese",
      "grade": 3,
      "term": 1,
      "item_count": 20,
      "total_ms": 204000,
      "path": "lessons/a1b2c3d4e5f6/lesson.json",
      "content_hash": "sha256:9f2c…",
      "published_at": "2026-11-06T18:00:00+08:00"
    }
  ]
}
```

- `path` 是**相对 dataBase 的路径**，不含域名。
- 课时字段与 `GET /api/lessons` 对齐（`id/title/subject/grade/term/item_count/total_ms`）；
  `grade`/`term` 可为 `null`，前端仅用于展示，不参与筛选逻辑。
- `dir` **不发布**：它是本机目录名，静态站不需要，也不应泄露主机组织方式。
- 写入时按 `title` 排序；播放器不依赖顺序。
- 该文件是**派生缓存**，不是账本：可由 `publish --reindex` 从 R2 重建（§6.5）。

### 4.2 `lessons/<id>/lesson.json`

与 `SessionManifest` 同形，播放器可直接消费。差异只有三处：

| 字段 | 规则 |
|---|---|
| `schema_version` | 新增，固定 `1`。前端不认识则拒绝渲染。与 SQLite 的 `user_version` 无关。 |
| `lesson_id` / `content_hash` | 新增，用于幂等与结果回传匹配。 |
| `subject` / `grade` / `term` | 与课时一致；`grade`/`term` 可为 `null`。前端用于标题旁展示。 |
| `session_id` | **省略**。静态模式由前端本地生成（`crypto.randomUUID()`）。 |
| `base` | **省略**。前端用 `new URL('.', lessonJsonUrl)` 推导；条目内也不得出现 `base`。 |
| `full_track` | 仅 `--with-full` 时出现，值为 `"full.mp3"`。 |

示例：

```json
{
  "schema_version": 1,
  "lesson_id": "a1b2c3d4e5f6",
  "content_hash": "sha256:9f2c…",
  "kind": "lesson",
  "title": "第5课",
  "subject": "chinese",
  "grade": 3,
  "term": 1,
  "defaults": { "gap_ms": 6000, "repeat": 2, "repeat_gap_ms": 1200,
                "lead_in_ms": 3000, "intro_gap_ms": 1500 },
  "intro": { "path": "clips/intro-ab12cd.wav", "duration_ms": 3300 },
  "intro_text": "请准备好纸和笔，听写马上开始。",
  "outro": { "path": "clips/outro-ef34ab.wav", "duration_ms": 2600 },
  "outro_text": "听写结束，请检查一遍。",
  "items": [
    { "index": 1, "text": "摇晃", "lang": "zh", "review": false,
      "clips": [{ "path": "clips/01-152a8adf.wav", "duration_ms": 1220 }],
      "repeat": 2, "repeat_gap_ms": 1200, "gap_ms": 6000 }
  ],
  "total_ms": 204000
}
```

- 静态导出**不包含复习词**（`review` 恒为 `false`）：复习依赖本地 SQLite，
  在静态站上无法工作。
- `intro` / `outro` / `intro_text` / `outro_text` 课时未配置时为 **`null`**（不省略字段）；
  前端对 null 与缺失同等处理。
- `text` 是 display 文本；`speak` 不落盘。

### 4.3 `content_hash` 定义

对以下**规范化 JSON** 做 `sha256`（键排序、无空白、UTF-8）：

```json
{
  "schema_version": 1,
  "title": "第5课",
  "subject": "chinese",
  "grade": 3,
  "term": 1,
  "defaults": { … },
  "intro": { "path": "…", "duration_ms": 3300 },
  "outro": { … },
  "items": [ { "index": 1, "text": "摇晃", "lang": "zh",
               "clips": [ { "path": "clips/01-152a8adf.wav", "duration_ms": 1220 } ],
               "repeat": 2, "repeat_gap_ms": 1200, "gap_ms": 6000 } ]
}
```

**排除**：`generated_at`、`content_hash` 自身、`total_ms`（可推导）。

含义：词表、顺序、间隔、语速/音量（体现在 clip 文件名）、音色或科目/年级任一变化
都会改变 hash；仅重新生成而内容不变则 hash 不变，发布被跳过。

---

## 5. 发布流程（增量、幂等、只增不删）

### 5.1 状态机

`dictation publish <课时>` / `POST /api/lessons/{id}/publish`：

```
1. 解析课时（目录名优先，其次 12 位 id）           → 404 if missing
2. 校验 lessons/<dir>/manifest.json 与 clips/    → 缺 clips 直接报错，不发布残缺课
3. 生成 lesson.json 载荷，计算 content_hash
4. GET  <dataBase>/lessons/<id>/lesson.json      → 1 次读
   ├─ 404            → 需要发布
   └─ hash 相同      → 跳过（除非 --force），exit 0
5. 逐 clip：HEAD clips/<name>.wav
   ├─ 200 → 跳过
   └─ 404 → PUT（Content-Type: audio/wav, Cache-Control: immutable）
6. PUT lessons/<id>/lesson.json                   → 提交点（no-cache）
7. GET lessons/index.json → merge 本课 → PUT      → 最后一步
```

HTTP 端可选 `{"async": true}`：立即返回 `202 {job_id}`，后台执行，复用
`GET /api/jobs/{id}`（`kind="publish"`）轮询；慢上行链路或 agent 客户端
有 60s 超时时用。

**顺序不可颠倒**：6 必须在 5 之后（否则列表里出现不完整课时），7 必须在 6 之后
（否则首页出现 404 的课）。

### 5.2 判定表

| 本地 | R2 上该课 | 动作 | 写入量 |
|---|---|---|---|
| 无 | — | 404 | 0 |
| 有，clips 缺 | — | 500/400，提示先 `dictation gen` | 0 |
| 有 | 无 | 全量：clips + lesson.json + index | ~24 |
| 有 | 有，hash 相同 | 跳过 | 0 |
| 有 | 有，hash 不同 | 增量 clips + lesson.json + index | 1 + 新增 clip 数 |
| 有（`--force`） | 有，hash 相同 | 重传 clips + lesson.json + index | ~24 |

“写入量”是对象级操作数（HEAD 不计为写）。重复发布仍是 22 次 HEAD + 1 次 GET，
**0 字节上传**。

### 5.3 删除与覆盖

- **永不删除**：重发、改名、修改参数都只新增 key。旧 clip 文件保留，
  正在收听的客户端不会 404。
- 同一 `<id>` 下 `lesson.json` 是唯一可被覆盖的对象。
- `unpublish` 只从 `index.json` 摘除条目，**音频默认保留**；`--purge` 才按前缀
  列举并删除（`DeleteObject` 在 R2 免费）。

### 5.4 并发

- 家庭场景只有一台家长机写入，进程内用 Mutex 串行；再加 `publish.lock` 文件
  防止 CLI 与 `serve` 同时发布。
- 不依赖 R2 的条件写/ETag：`index.json` 采用 read-modify-write，单写者前提下安全。
- 同一课重复触发以先完成者为准；后到的若 hash 相同则跳过。

### 5.5 失败与重试

| 阶段失败 | 状态 | 重试 |
|---|---|---|
| clip PUT 失败 | 部分 clip 已在 R2，`lesson.json` 未更新 | 幂等，直接重跑 |
| `lesson.json` PUT 失败 | 新 clip 已在，旧 `lesson.json` 仍生效 | 重跑 |
| `index.json` PUT 失败 | 课已发布但未出现在列表 | 重跑（或 `--reindex`） |

错误响应不返回堆栈，只返回单行中文原因 + 阶段标记
（`{"error":"…","stage":"clips|commit|index","uploaded":22,"committed":false}`）。

---

## 6. 本地状态与“全量”保证

### 6.1 不依赖本地历史

`index.json` 的更新是 **GET（从 R2）→ merge → PUT**，不是从本地目录重建。
因此：

- 本地只剩刚导出的一课 → 其他条目原样保留；
- 本地旧课音频被 `cache/` 配额清理 → 完全不会被读取；
- 本地 `lessons/` 被删 → CF 上无变化。

### 6.2 不需要本地发布状态文件

**禁止**引入 `publish.json` 之类的本地账本作为判断依据。判定一律以 R2 上的
`lesson.json` + `content_hash` 为准，避免本地与远端不一致时做出错误决策。

（可选：本地缓存 `lessons/<id>/.publish.json` 仅用于 `--dry-run` 时少发一次
HEAD 请求，但**不得**影响正确性，缺失时必须回落到远端判定。）

### 6.3 dry-run 约定

发布是**可重复且不破坏**的操作，因此 `publish` 默认执行；`--dry-run` 显式
请求“只打印将要做的事”。`unpublish --purge` 是破坏性操作，**默认 dry-run，
必须 `--apply`**。

### 6.4 校验“CF 上是全量”

- `publish --status --json`：读 `index.json`，逐条 HEAD `lesson.json`，
  报告 `missing`（列表有、对象无）与 `orphan`（对象有、列表无）。
- 正常情况下两者**都应为空**；出现即说明上次发布中断，重跑即可修复。

### 6.5 重建索引

`publish --reindex`：

1. `LIST lessons/` 前缀（分页，1000/页）；
2. 读取每个 `lessons/<id>/lesson.json`（可按需并发 4）；
3. 重新生成 `index.json` 并 PUT。

用于：index 被写坏、换域名/换桶、批量清理后。**不读本地任何文件。**

---

## 7. 配置与凭据

### 7.1 `config.toml`

```toml
[publish]
enabled = false                       # 未配置时为 false，publish 返回 503
data_base = "https://audio.example.com"   # 发布后前端访问音频的根
pages_base = "https://dictation.example.com"  # 仅用于拼接分享链接
bucket = "dictation"
# 以下可选；默认见 §7.3
pages_project = "dictation"
shell_dir = "dist/shell"
```

### 7.2 环境变量（`/etc/dictation/env`，0600）

```bash
R2_ACCOUNT_ID=...
R2_ACCESS_KEY_ID=...
R2_SECRET_ACCESS_KEY=...
# 可选，覆盖 config.toml
R2_BUCKET=dictation
R2_DATA_BASE=https://audio.example.com
```

- 由 `serve` 读取（沿用 `$CREDENTIALS_DIRECTORY` → 环境变量回退，同 D13b）。
- **agent 不持有、不接触**这些值；agent 只调 HTTP。
- CLI（运维/本地调试）可以读同一个 env 文件，与 TTS key 的处理一致。

### 7.3 未配置时

`publish` 与 `POST /api/.../publish` 返回 `503 {"error":"未配置发布目标（config.toml [publish]）"}`，
不影响本地播放与生成。

---

## 8. 静态前端模式

同一份 `web/` 支持两种模式。**不引入构建步骤拆分代码**（AGENTS.md：前端纯静态、
无构建），静态能力在运行时按配置收口。

`index.html` 在 `app.js` 之前加载一次配置：

```html
<script src="config.js"></script>
<script src="app.js" defer></script>
```

- 仓库内 `web/config.js` 默认 `{ "mode": "server" }`，`serve` 直接可用；
- `publish --shell` 生成 `dist/shell/config.js`：
  `{ "mode": "static", "dataBase": "https://audio.example.com/" }`；
- 文件缺失、解析失败或 `mode !== "static"` → 一律按 `server` 处理，**不隐式降级**。

### 8.1 API 能力表

| 调用（`web/app.js` 的 `API`） | server | static |
|---|---|---|
| `lessons()` | `GET /api/lessons` | `GET <dataBase>/lessons/index.json` |
| `createSession(lessonId)` | `POST /api/sessions` | `GET <dataBase>/lessons/<id>/lesson.json`；本地补 `session_id`、`base` |
| `check(sessionId, results)` | `POST /api/sessions/{id}/check` | 本地判分，直接进结果页 |
| `reviewSummary()` | `GET /api/review/summary` | `{ "due_count":0,"wrong_count":0,"review_count":0,"subjects":[] }` |
| `reviewLessons()` | `GET /api/review/lessons` | `{ "lessons": [] }` |
| `createReview(subject)` | `POST /api/sessions/review` | 不支持（入口不渲染） |
| `lessonReview(subject, id)` | `POST /api/sessions/review` | 不支持（卡片不显示复习角标） |
| `getSession(id)` | `GET /api/sessions/{id}` | 不支持（`?s=` 忽略） |
| `regenerateWord(id, text)` | `POST /api/lessons/{id}/regenerate-word` | 不支持（长按不绑定） |
| `job(id)` | `GET /api/jobs/{id}` | 不使用 |

### 8.2 界面能力矩阵

| 前端功能 | 实现 | server | static | 说明 |
|---|---|---|---|---|
| 时间轴、暂停/继续 | 纯前端 | ✅ | ✅ | — |
| 上一个 / 下一个 | `jumpTo()` | ✅ | ✅ | 纯本地跳转 |
| 重听这个词 | 纯前端 | ✅ | ✅ | — |
| 断点续听 | localStorage | ✅ | ✅ | 按课时区分；static 校验 `content_hash`（§8.4） |
| 听写节奏（隐藏，长按标题） | localStorage | ✅ | ✅ | 只改播放间隔，不写服务端 |
| 检查页 / 结果页 | DOM | ✅ | ✅ | 判定来源不同（8.1） |
| 课内复习角标与入口 | `/api/review/lessons` | ✅ | ❌ | 依赖 SQLite |
| 错词复习卡片 | `/api/review/summary` | ✅ | ❌ | 同上 |
| 长按重掷词 | `/api/lessons/{id}/regenerate-word` | ✅ | ❌ | 静态无 TTS |
| 会话深链 `?s=` | `GET /api/sessions/{id}` | ✅ | ❌ | 静态改用 `?l=` |
| 导出结果 | 新增 | 可选 | ✅ | 见 §9.1 |

**静态模式必须做到“不发起任何 `/api/*` 请求”**：标 ❌ 的入口在**渲染阶段**就
不创建节点，而不是点了才报错。

### 8.3 深链

- static：`https://dictation.example.com/?l=<lesson_id>` 直接加载该课；
  `l` 缺失 → 首页列表；指向不存在的课 → 回落列表 + toast「这一课可能已下线」。
- server：保留现有 `?s=<session_id>`（分享已结束的 session）。
- 同时出现时 server 模式以 `s` 优先；static 模式忽略 `s`。

`index.json` 的 `path` 相对索引 URL 解析：`new URL(entry.path, indexUrl)`。

### 8.4 静态模式明确不做

- 复习（错词 / 课内 / 跨课）——依赖 SQLite 与 `review_state`。
- 断点续听进度校验：static 模式用 `lesson.json` 的 `content_hash` 与
  `index.json` 比对，重新发布（hash 变化）即清除本地进度；server 模式续播
  按当前课时新建会话（见 `player.md` 断点续听）。
- 重掷词——依赖 TTS 与写盘。
- 结果入库——改为导出 JSON，回家导入（§9）。
- 家长端写操作（导入 / 生成 / 删除）——全部留在本地。

### 8.5 统一 URL 出口（为以后的口令预留）

静态模式下访问 R2 的 URL **只允许**由 `dataUrl(path)` 生成：

```js
function dataUrl(path) {
  return new URL(path, DATA_BASE).href;   // v1 不追加任何参数
}
```

- `lessons()`、`createSession()`、`mediaUrl()`/`clipSrc()` 都必须经过它；
  不得在别处手拼 `dataBase + path`。
- `dataBase` 可以是同源相对路径（`/data`），B 方案的 Function 代理默认用它；
  绝对 URL 与相对路径都支持。
- `base`（顶层与 item）保持干净，不带查询参数——它会被写进 localStorage 的
  断点续听快照；带参数会导致口令轮换后旧进度全部失效。
- 以后加口令时只改这一个函数（追加 `?k=`）并新增口令页，数据格式与发布流程
  不变。

---

## 9. 结果回传（闭环）

### 9.1 导出格式 `dictation.result/v1`

```json
{
  "schema": "dictation.result/v1",
  "lesson": {
    "lesson_id": "a1b2c3d4e5f6",
    "content_hash": "sha256:9f2c…",
    "title": "第5课",
    "subject": "chinese",
    "url": "https://audio.example.com/lessons/a1b2c3d4e5f6/lesson.json"
  },
  "session": {
    "kind": "lesson",
    "started_at": "2026-11-06T10:00:00+08:00",
    "finished_at": "2026-11-06T10:05:12+08:00",
    "device": "iPad",
    "student": ""
  },
  "items": [
    { "position": 1, "text": "摇晃", "lang": "zh", "review": false, "correct": false }
  ],
  "summary": { "total": 20, "correct": 19, "wrong": 1 }
}
```

- **自包含**：`items[].text` 足以入库，不依赖服务端能否匹配到课时。
- `lesson.subject` **必填**：词的身份是 `(subject, text)`，缺科目会把词写进错误的
  命名空间（语文/英语同形词串台）。
- `content_hash` 仅用于提示“这次听的是旧版本”。
- `student` 可选，默认空串（自家孩子）。

**移动端导出顺序**（三级降级，缺一不可）：

1. `navigator.canShare({files})` → `navigator.share`（iOS 分享面板）；
2. `Blob` + `<a download>`，文件名 `<title>-<YYYYMMDD>.json`；
3. 只读 textarea + 全选/复制（覆盖微信内置浏览器）。

### 9.2 导入

**幂等键**：服务端对
`{lesson.content_hash, lesson.subject, session.finished_at, session.student,
items[].position/text/correct}` 的规范化 JSON 求 `sha256`，
**忽略客户端可能提供的任何 id**。实现上 `finished_at` 先解析为 Unix 秒，
`items` 按 position 排序，保证同一份结果跨时区/字段顺序稳定。

- 已导入过 → `200 {"status":"duplicate","session_id":"…"}`，不重复写 attempts/lapses。
- 时间基准用 `session.finished_at`（不是 `now`），否则旅途中的错词会被推迟复习。
  未来时间 > 5 分钟则钳制为 `now`。
- 落库：新建 `kind="imported"` 的 session + `session_items` + `attempts`
  （`source="import"`）+ `review_state`，一个事务内完成；幂等键存 `result_imports`。
- 导入时按 `lessons/*/manifest.json`（同科目）建立文本→片段索引：
  - 本机已有该词：沿用原音源；
  - 本机还没有但课时就在本机：用清单里的片段补上（新课时没在家做过也能复习）；
  - 都没有（外来课时）：片段留空，记录可查但**不进复习池**。
- 入库后 `result list/show/wrong` 无需改动即可查看；错词立即进入首页复习入口。

HTTP：

```
POST /api/results/import
  body: <dictation.result/v1>
  200 { "status":"imported"|"duplicate", "session_id":"…",
        "correct":19, "wrong":1, "review_count":3 }
  400 { "error":"…" }        校验失败
  409 { "error":"…" }        同一 finished_at 但内容不同（疑似被改）
```

CLI：

```
dictation result import <file|-> --json
```

首页/家长页的「导入结果」入口复用同一实现，提供“选文件”和“粘贴 JSON”两种方式。

### 9.3 校验规则

- `schema` 必须完全匹配；`lesson.subject` ∈ `chinese|english`。
- `items` 非空且 ≤ 200 条；`position` 唯一。
- `text` 长度 ≤ 200，去首尾空白；空文本拒绝。
- `correct` 必须是布尔值。
- 未知字段忽略（前向兼容）；缺少 `summary` 时按 `items` 重算。

---

## 10. CLI 契约

已实现：

```bash
dictation publish <课时> [--dry-run] [--force] [--with-full] [--json]
dictation publish [--json]              # 省略课时 = 发布全部课时
dictation publish --shell [--dry-run] [--json]   # 生成 Pages 工程（不需要 R2 密钥）
dictation result import <file|-> [--json]
```

- `<课时>` 解析顺序与 `generate` 一致：目录名优先，其次 12 位 id。
- 未配置 `[publish]` 时：CLI 退出码 1（`--json` 输出 `{"error":…}`），
  HTTP 返回 503。
- `publish` 成功输出（单课时）：

```json
{ "lesson_id": "a1b2c3d4e5f6", "title": "第5课",
  "status": "published|skipped",
  "dry_run": false,
  "uploaded": 0, "skipped": 22,
  "index_updated": true,
  "url": "https://dictation.example.com/?l=a1b2c3d4e5f6" }
```

- 发布全部或部分失败时输出 `{ "results": [...], "failures": [{ "lesson", "stage", "error", "uploaded", "committed" }] }`，退出码 1。
- `url` 由 `config.toml` 的 `pages_base` 拼接；未配置则无该字段。

`--shell` 输出（默认 `dist/pages`）：

```
Pages 工程已生成 dist/pages（9 个文件）
dataBase：https://dictation.example.com/data
bucket  ：dictation（binding BUCKET）
部署：cd dist/pages && npx wrangler pages deploy
```

未实现（后续阶段）：

```bash
dictation publish --status     # 校验 index 与对象一致性（missing/orphan）
dictation publish --reindex    # 从 R2 重建 index.json
dictation unpublish <课时> [--purge] [--apply]
```

---

## 11. Cloudflare 侧配置

### 11.1 域名

- Pages 项目：`dictation.example.com`（Direct Upload + 自定义域）。
- R2 桶：**不绑自定义域**，保持私有；`r2.dev` 保持关闭。
- 公开可寻址的只有 Pages 一个域名。

### 11.2 R2 binding（`wrangler.jsonc`）

Pages Function 通过 binding 读私有桶，配置由 `--shell` 生成：

```jsonc
{
  "name": "dictation",
  "pages_build_output_dir": "./public",
  "compatibility_date": "2026-11-20",
  "r2_buckets": [
    { "binding": "BUCKET", "bucket_name": "dictation" }
  ]
}
```

Function 侧只做 key 映射与缓存头（不鉴权、不列目录）：

```js
// /data/lessons/index.json -> 桶内 lessons/index.json（不重复加前缀）
const key = (params.path || []).join('/');
if (!key.startsWith('lessons/')) return new Response('not found', { status: 404 });
const object = await env.BUCKET.get(key);
return new Response(object.body, { headers });
```

因为同源，**不需要 CORS**。若改回 A 方案（公开桶），才需要给 `clips/*` 配 CORS。

### 11.2.1 额度

- Pages 静态资源不消耗 Workers 请求；
- 只有 `/data/*` 每次请求 = 1 次 Function 调用；Free **10 万/天**；
- 一课 40 词 ≈ 43 次请求（42 片段 + `lesson.json`）；
- 40 人每天各听一次 ≈ 1.8k/天（1.8%）。

### 11.3 缓存

| 对象 | 上传时 `Cache-Control` | 说明 |
|---|---|---|
| `clips/*.wav` | `public, max-age=31536000, immutable` | 文件名含内容哈希 |
| `lesson.json` | `no-cache` | 重发要立即生效 |
| `index.json` | `no-cache` | 新发布要立即出现在首页 |

可选 Cache Rule（只匹配 `http.request.uri.path contains "/clips/"`，Cache Everything）：
重复播放不再回源 R2。默认缓存不覆盖 `.wav`，需要显式配置。

### 11.4 Pages `_headers`

```
/*
  Referrer-Policy: no-referrer
/*.js
  Cache-Control: public, max-age=300
/*.css
  Cache-Control: public, max-age=300
```

（壳的资源目前没有内容哈希，先给短缓存；将来若加哈希再改 `immutable`。）

### 11.5 口令（v1 不启用，只留出口）

**决定**：v1 不加访问控制。理由是超额的代价有上限（`$0.36/百万`），且真正的
拦截只能在边缘做，前端输入框只是体验层。

但前端必须现在就留好出口，避免以后改契约：

- 所有 `<dataBase>` 上的 URL（`index.json`、`lesson.json`、clips）**只能**由
  一个函数生成，例如 `dataUrl(path)`；其它地方不得手拼。
- `dataUrl()` 在 v1 原样返回，不追加任何参数。
- **`base` 字段永远保持干净**（不带 `?k=`），否则口令轮换后断点续听与
  `lesson.json` 的相对路径全部失效；token 只在 `dataUrl()` 里临时拼。

以后启用时的配置：

```
(http.host eq "audio.example.com" and not http.request.uri.query contains "k=<TOKEN>")
→ Block
```

- 被拦请求在边缘终止，**不产生 R2 读操作**。
- 前端：URL 的 `?k=` → 写入 localStorage → `dataUrl()` 追加；无 `?k=` 时显示
  口令页，首次拉 `index.json` 的 200/403 即校验结果。
- 轮换只改 WAF 规则 + 分享链接 + `config.toml [publish] token`，**无需重传数据**。
- 免费版限制：自定义规则 5 条（够用）、不支持正则、不支持
  `is_timed_hmac_valid_v0` 时间签名（需 Pro+）。WAF 按 zone 套餐计费，
  **没有按请求数计的配额**。

---

## 12. 额度测算（免费版）

| 项 | 数值 | 占免费额度 |
|---|---|---|
| 单课 clips | 1.43 MB / 22 个对象 | — |
| 一次听写请求 | ~22 次（`index.json` + `lesson.json` + 20 clips） | — |
| 40 人 × 每天 1 次 × 30 天 | ~2.6 万次 B 类 | 0.26%（上限 1000 万） |
| 发布 100 课 | 2,400 次 A 类 | 0.24%（上限 100 万） |
| 存储 100 课 | ~145 MB | 1.4%（上限 10 GB） |

出站流量免费。**请求量不是瓶颈**；最先触顶的是 Pages 的 2 万文件/站点
（仅当选择把音频放 Pages 时才相关，本方案不涉及）。

---

## 13. Skill 更新（实施时必须完成）

`skills/dictation-prep/SKILL.md` 目前只覆盖“导入 → 生成”。发布上线后必须补：

- [x] 流程改为 **导入（必带 `subject`）→ 生成 → 轮询 job → 发布 → 返回分享链接**；
- [x] 明确 agent **不执行 CLI、不接触 R2 凭证、不读写任何路径**，只调
      `POST /api/lessons`、`POST /api/lessons/{id}/generate`、
      `GET /api/jobs/{id}`、`POST /api/lessons/{id}/publish`；
- [x] 失败分支：`503`（未配置发布目标）时提示家长在 `config.toml` 补
      `[publish]`，不要重试；`404` 时提示先导入/生成；
- [x] 幂等说明：重复发布返回 `skipped`，不要当作错误；
- [x] 输出给家长的内容：课时标题 + 分享链接（`?l=<id>`）+ 是否含整轨；
- [x] 明确“发布不改变本地文件、不重新合成、不消耗 TTS 额度”。

同步检查：

- [ ] `AGENTS.md` 增加一条设计决策（分享发布 = 壳/数据双通道，R2 为真相源），
      只在实施时改，不在本文件里预先断言编号；
- [ ] `README.md` / `INSTALL.md` 补 Cloudflare 一次性配置步骤；
- [ ] `TODO.md` 记录进度（等当前并行会话结束后再改，避免冲突）。

---

## 14. 测试计划

**单元**

- `content_hash`：稳定、与 `generated_at` 无关、clip 改名会变；
- `index.json` merge：远端有条目时本地不留历史也不丢条目；
- key 拼接：`lesson_id` 与 `clips/<name>` 组合无重复斜杠、无 `..`；
- `result/v1` 幂等键：同一 payload 稳定，改 `correct` 会变。

**集成（本地 mock S3）**

- 连续发布两次：第二次 `uploaded=0`，`status=skipped`；
- 远端 index 不存在时从空开始；存在时保留未知条目；
- 删除本地 `lessons/` 后 `--reindex` 能重建；
- 中途注入 500：重试后状态一致。

**HTTP**

- 未配置 `[publish]` → 503；
- 未知课时 → 404；
- 发布中重复请求 → 串行/幂等返回；
- 鉴权：设置 `DICTATION_API_TOKEN` 后，发布端点缺 token → 401（且优先于 body 解析）。

**E2E（手动，不进 CI）**

- 发布一课 → 手机 4G 打开分享链接 → 完整听写 → 标错 → 导出 JSON；
- 回家导入 → 首页出现「错词复习 (N)」→ 下一课自动混入。

---

## 15. 待定（实施时确认）

已实现：`[publish]` 配置 + R2 S3 客户端（SigV4 自签名，`hmac` 纯 Rust）、
`dictation publish`（单课/全部/dry-run/force/with-full/json）、
`publish --shell`（B 方案 Pages 工程：静态壳 + R2 代理 Function + binding）、
`POST /api/lessons/{id}/publish`（同步、鉴权、503/404/400/502 带 stage）。

仍待实施 / 待定：

1. `publish.lock`：跨进程（CLI 与 `serve` 同时发布）互斥；当前仅 HTTP 端进程内串行。
2. 实际部署 Pages 仍用 `wrangler pages deploy`（`--shell` 只生成工程，不联网）。
3. `publish --status` / `--reindex`（LIST 分页）、`unpublish`（index 摘除 + `--purge`）。
4. R2 凭证是否复用 `/etc/dictation/env`（倾向是，已按 `read_secret` 实现），
   systemd 下是否用 `LoadCredential` 注入。
5. 家长页「导入结果」入口放在首页还是独立页。
6. 真实 R2 冒烟：填入凭证后跑一次 `publish --dry-run` → `publish` → 浏览器打开分享链接；
   通过后在 `TODO.md` 勾掉本项。
