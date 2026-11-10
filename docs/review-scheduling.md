# 复习组卷与科目隔离设计

> 状态：实施中。第 1–4 步已完成（迁移/科目、组卷、加练、AI 组卷）；
> 第 5 步（文档/skill 收尾 + 发布）待做。
> 关联：[`http-api.md`](http-api.md)（接口契约）、[`player.md`](player.md)（播放与检查页）、
> [`AGENTS.md`](../AGENTS.md)（D10 错词复习、§10 记忆曲线）。
> 最后更新：2026-11-09

## 1. 目标

- 复习必须**按科目严格隔离**：语文和英语各有独立的词库、积压和复习会话。
- 组卷有明确规则：到期、优先级、上限、顺序，不再使用写死的「最多 5 个」。
- 支持**加练**：没有新词的日子可以单独做复习，积压可主动消化。
- 支持 **AI 组卷**：agent 读取待复习池，挑词/排序，生成一个临时复习课时
  （如「第一单元综合复习」），其后续批改与调度与应用内复习完全一致。
- 本轮**不引入 FSRS**；保留 `attempts`、`due_at`、`lapses` 等数据，后续可迁移。

非目标：

- 不做多孩子 / 多账号。
- 不做跨设备同步。
- 不做按教材单元的自动范围识别（单元由家长/AI 在标题或筛选条件里表达）。

## 2. 数据模型

### 2.1 科目与年级

| 字段 | 位置 | 规则 |
|---|---|---|
| `subject` | `lessons`、`sessions`、`words`、manifest | 必填；代码值 `chinese` / `english`，界面显示「语文 / 英语」 |
| `grade` | `lessons`、`sessions`、`words` | 可选，整数 1–6；仅用于筛选与展示 |
| `term` | 同上 | 可选，1 / 2（上/下）；与 `grade` 一起表达「三年级上」 |

**词的身份 = `(subject, text)`**，不是全局 `text`：

- 语文的「行」和英语的“line”互不干扰；同形不同科目也不共享记忆状态。
- `grade`/`term` **不进唯一键**：同一个词跨年级仍共用一条记忆曲线；
  年级只用于「本阶段复习范围」筛选（例如寒假只看三年级上）。

`lang`（`zh`/`en`/`mixed`）仍然保留，表示**发音语言**，与科目解耦：
英语课里可以有中文释义，语文课里可以出现英文缩写。

### 2.2 manifest

`manifest.json` 增加可选字段：

```json
{ "subject": "chinese", "grade": 3, "term": 1 }
```

- 缺省 `subject="chinese"`、`grade=null`、`term=null`；旧 manifest 不受影响。
- `version` 保持 `1`：这是可选字段追加，不是破坏性变更。

### 2.3 科目推断仅用于迁移

规则只有一条：

- `text` 中含**拉丁字母**（`A-Z`/`a-z`）→ `english`
- 其他（汉字、数字、符号、纯标点）→ `chinese`

不做 Han 检测、不做混合判断：当前数据全中文，英语词必然含拉丁字母。
**运行时不以字符集推断科目**，只在 v1→v2 迁移时回填；导入/生成必须显式指定
`subject`（见 §7）。

## 3. schema 变更流程

### 3.1 版本号

用 SQLite 内置的 `PRAGMA user_version`，当前 schema 视为 **v1**，本次升到 **v2**。
不额外建迁移表。

### 3.2 迁移执行器

`Db::open` 时：

1. 读 `user_version`；
2. 大于程序支持版本 → **拒绝启动**，提示升级二进制（防止旧程序写坏新库）；
3. 小于支持版本 → 逐条执行迁移：
   - 迁移前做一致性备份：`VACUUM INTO 'dictation.db.bak-v{from}-{YYYYMMDDHHMMSS}'`；
     只保留最近 **3** 份，旧的按时间删除；
   - `PRAGMA foreign_keys=OFF` → `BEGIN` → 执行 → `foreign_key_check` → `COMMIT`
     → `PRAGMA foreign_keys=ON`；
   - 成功提交后写 `PRAGMA user_version = {to}`；
   - 任何一步失败 → 回滚并报错，服务不启动（不会带着半迁移的库跑）。
4. 迁移函数**只追加、不修改**；已发布的迁移视为不可变。

### 3.3 v1 → v2 → v3

```sql
ALTER TABLE lessons  ADD COLUMN subject TEXT NOT NULL DEFAULT 'chinese';
ALTER TABLE lessons  ADD COLUMN grade INTEGER;
ALTER TABLE lessons  ADD COLUMN term INTEGER;
ALTER TABLE sessions ADD COLUMN subject TEXT NOT NULL DEFAULT 'chinese';
ALTER TABLE sessions ADD COLUMN grade INTEGER;
ALTER TABLE sessions ADD COLUMN term INTEGER;
```

`words` 的唯一键要改，SQLite 不能直接改约束，需重建：

```sql
CREATE TABLE words_v2 (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  subject TEXT NOT NULL,
  text TEXT NOT NULL,
  lang TEXT NOT NULL,
  clip_lesson TEXT,
  clip_path TEXT,
  duration_ms INTEGER,
  gap_ms INTEGER,
  repeat INTEGER,
  updated_at INTEGER NOT NULL,
  UNIQUE (subject, text)
);
```

回填在 Rust 里逐行完成（SQLite 没有正则）：

1. 读旧 `words` 每行，按 §2.3 推断 `subject`；
2. 用**相同的 `id`** 插入 `words_v2`，保证 `attempts.word_id` / `review_state.word_id` 不变；
3. 删除旧表、改名、重建索引。

其他回填：

- `lessons.subject`：全部 `chinese`（当前课时均为语文）；启动时 warn 列出待人工确认的课时。
- `sessions.subject`：有 `lesson_id` 的跟随课时；复习会话默认 `chinese`。

v2 → v3：`session_items` 增加 `repeat_gap_ms`（默认 1200），
用于 `GET /api/sessions/{id}` 精确重放。

### 3.4 向后兼容规则

- 迁移只增不删；新列必须有默认值或可空。
- 读取端对可选字段一律给默认值（`serde(default)`），旧 manifest 仍可播放。
- API 只做**字段追加**；删除/改名字段必须同步契约、skill、README、PWA。
- **写请求的 `subject` 必填**（导入、复习、AI 组卷）；旧 PWA 与旧 skill
  必须在同一版本一起更新，不保留「缺省语文」的兼容分支。
- 数据库迁移仍然把旧数据回填为 `chinese`，保证升级后已有课时/词可用。

## 4. 复习调度

### 4.1 到期与进阶（不变）

- 到期：`last_grade='again' OR due_at <= now`。
- 答对：`1→2→4→7→15→30` 天，从**本次批改时间**起算。
- 答错：重置 1 天、`lapses+1`。
- 逾期答对照常升一级，不惩罚（拖延不是遗忘的证据）。

### 4.2 选词优先级

同科目内，排除本课已有的词、没有 clip 的词：

1. `last_grade='again'` 优先；
2. 其余按**距上次复习时间**从长到短（`last_review_at = due_at - interval_days*86400`）；
3. 平手按 `due_at`、`word_id`。

理由：记忆衰减取决于「多久没见过」，而不是「逾期了几天」。

### 4.3 数量

```
count = min(review_cap, max(3, ceil(new_count * 0.25)))
```

- `review_cap` 默认 **8**，`0` 表示本课不混复习，最大 15。
- 时间护栏：复习段音频 ≤ **150s**，超出则少取。
- `review_cap` 写入 `manifest.review_cap`（顶层字段，默认 8），服务端从 manifest 读取。

时长参考（默认 `repeat=2`、`gap=6s`、实测单词片段约 1.2s，每个词约 9.7s）：

| 组成 | 时长 |
|---|---|
| 30 个新词 | 约 5:01 |
| 8 个复习词 | 约 1:18 |
| 合计 | **约 6:19** |

`gap` 就是书写时间，所以音频时间轴约等于实际听写时长（不含重听/暂停）。
四字成语每个约多 0.8–1.6s，影响很小。

### 4.4 位置与顺序

- 位置：追加在本课之后、outro 之前（不变）。
- 顺序：**选词按优先级，播放顺序按 session 随机种子洗牌**
  （`sha256(seed + text)`）。
  - 同一次 session 稳定（顺序已写进 session_items，刷新/断点续听不变）；
  - 不同次顺序不同，避免「粗壮→摇晃」固定搭配形成位置记忆；
  - 新词顺序不动，只打乱复习段。

## 5. 积压与加练

### 5.1 汇总

`GET /api/review/summary` 增加按科目的分组（保留旧字段以兼容）：

```json
{
  "due_count": 5, "wrong_count": 3, "review_count": 6,
  "subjects": [
    { "subject": "chinese", "due_count": 4, "wrong_count": 2,
      "review_count": 5, "oldest_overdue_days": 6 },
    { "subject": "english", "due_count": 1, "wrong_count": 1,
      "review_count": 1, "oldest_overdue_days": 1 }
  ]
}
```

### 5.2 复习会话（加练）

`POST /api/sessions/review`：

```json
{ "subject": "chinese", "limit": 20, "grade": 3 }
```

- `subject` 必填（多科目下不允许含糊）；`limit` 默认 20、最大 40；
- `grade`/`term` 可选，仅筛选范围；按 `words.clip_lesson → lessons.grade/term`
  过滤（即词的来源课），不额外存字段；
- 选词规则同 §4.2；`kind='review'`、`title='错词复习'`、无 intro/outro。
- 首页按科目显示入口；家长模式可选「练 10 / 20 / 全部（≤40）」。

## 6. AI 组卷

### 6.1 读取待复习池

`GET /api/review/queue?subject=chinese&limit=200&grade=3`

```json
{
  "subject": "chinese",
  "count": 42,
  "words": [
    { "text": "粗壮", "lang": "zh", "last_grade": "again", "lapses": 2,
      "due_at": "2026-11-08T10:00:00+08:00", "interval_days": 1,
      "last_review_at": "2026-11-07T10:00:00+08:00", "overdue_days": 3 }
  ]
}
```

按 §4.2 排序，agent 可以直接取前 N 个，也可以按自己的教学判断重排。

### 6.2 创建临时复习课时

`POST /api/sessions/custom`：

```json
{
  "subject": "chinese",
  "title": "第一单元综合复习",
  "grade": 3,
  "items": ["粗壮", "摇晃", "旅行"]
}
```

- 不创建 `lessons/` 目录、不调用 TTS；clip 来自 `words` 表；
- **顺序由请求决定**（agent 可能有意安排），服务端不洗牌；
- 校验：
  - `subject` 必填，缺失 → `400 {"error":"subject 必填（chinese|english）"}`；
  - `items` 1–**60** 个；超过 → `400 {"error":"复习词过多（最多 60 个，本次 N 个）","limit":60,"got":N}`；
  - 去重保序；重复 → `400 {"error":"重复的词","duplicated":[...]}`；
  - 未知词 → `400 {"error":"未知词","unknown":[...]}`；
  - 没有 clip → `400 {"error":"缺少音频","unplayable":[...]}`；
- 返回 `SessionManifest`：`kind="review"`、`title` 为请求标题、`review=true`、
  `lead_in=3000`、无 intro/outro；
- 之后走普通播放 → 检查提交，`attempts` 与 `review_state` 更新与自动复习**完全一致**。

### 6.3 打开与续听

- `GET /api/sessions/{id}` 返回已创建 session 的 manifest（支持断点与分享）。
- PWA 支持 `?s=<session_id>` 深链，直接进入该会话。
- 首页可选显示最近未完成的临时复习会话（后续）。

### 6.4 与结果导入的区别

| 场景 | 入口 | 词匹配 | 调度 |
|---|---|---|---|
| AI 挑词组卷（本设计） | `POST /api/sessions/custom` | 必须已存在 | 批改时正常更新 |
| 离线结果回传（分享功能） | `POST /api/results/import` | 按 `(subject, text)` 匹配，缺失可建词或标记 `unmatched` | 用 `finished_at` 为时间基准写入 |

两者都**不允许 agent 直接写 `due_at`**，调度权始终在服务端。

## 7. 兼容性清单

- 旧 manifest 无 `subject` → 读取为 `chinese`，可正常播放。
- **写请求的 `subject` 必填**（`POST /api/lessons`、`/api/sessions/review`、
  `/api/sessions/custom`）；旧 PWA 与旧 skill 与本次发布同步更新。
- CLI `lesson import` 同样要求 `--subject`；`lesson gen` 从 `lesson.toml` 读取，
  手写旧课时缺省 `chinese`（仅读取兼容，不用于新导入）。
- 数据库迁移把旧课时/词/会话回填为 `chinese`，升级后数据可直接用。
- `GET /api/lessons` 追加 `subject`/`grade`/`term`，不删旧字段。
- 结果 JSON 追加 `subject`；旧字段不变。
- CLI 与 HTTP 语义同步（见 `http-api.md` 的一致性约定）。

## 8. 测试计划

迁移：
- 用代码构造 v1 库（含中文词、英文词、attempts、review_state）→ 打开 → 断言：
  `user_version=2`、词 id 不变、`subject` 回填正确（含拉丁字母 → `english`）、
  外键完整；
- 重复打开幂等；`user_version` 高于程序 → 拒绝启动；
- 同一 text 在中英两科下可共存（唯一键生效）；
- 备份只保留最近 3 份。

组卷：
- 优先级：`again` 先于到期词；最久未复习优先；
- 数量：20→5、30→8、40→8、`cap=0`→0、小课时按 3 保底；
- 150s 护栏生效；
- 洗牌：同 session 稳定、不同 session 不同；
- 科目隔离：语文到期词不出现在英语会话。

AI 组卷 / 加练：
- 自定义会话顺序保持、超 60 个 / 重复 / 未知 / 缺音频分别 400 且错误可读；
- 批改后 `review_state` 与应用内复习结果一致；
- `limit` 默认/上限、科目筛选、年级/学期筛选；
- 结果查询按科目过滤。

## 9. 实施顺序

1. **迁移框架 + v2 + 科目字段贯通**（manifest、导入、列表、结果）。
2. **组卷算法**：manifest `review_cap`、优先级、数量、洗牌、150s 护栏。
3. **积压汇总 + 加练会话**（API + 首页分组；家长模式可后置）。
4. **AI 组卷**：`/api/review/queue`、`/api/sessions/custom`、`GET /api/sessions/{id}`、PWA 深链。
5. 文档/skill 同步，全量验证，`v0.2.0-rc.1`。

## 10. 已确认的决策

- 年级字段：`grade`（1–6）+ `term`（1/2）。
- 科目：`chinese` / `english`，写请求必填。
- 迁移回填：含拉丁字母 → `english`，其余 → `chinese`。
- 组卷：`min(review_cap=8, max(3, 25%×新词数))` + 150s 护栏。
- 加练：`limit` 默认 20、最大 40。
- AI 组卷：上限 60 个词，超出直接报错。
- 备份：保留最近 3 份。
- FSRS：本轮不做。

## 11. skill / 文档同步（实施时必须完成）

`skills/dictation-prep/SKILL.md`：

- 导入 payload 增加 **必填** `subject`（`chinese`/`english`），可选 `grade`、`term`；
  写明「旧请求不带 subject 会 400，不是默认语文」。
- 新增「复习组卷」流程：
  1. `GET /api/review/queue?subject=...` 取待复习池（已按优先级排序）；
  2. agent 挑选/排序，`POST /api/sessions/custom` 创建临时复习课时；
  3. 把 `?s=<session_id>` 链接给家长。
- 明确 `POST /api/sessions/custom` 的 **60 词上限**：
  超过时返回 `{"error":"复习词过多（最多 60 个，本次 N 个）"}`，
  agent 必须裁剪到 60 以内，或请家长决定分几次复习；**不要原样重试**。
- 说明 AI 只决定「选哪些词、什么顺序」，**不写调度**；对错与下次复习时间
  由应用批改后统一计算。

`docs/http-api.md`：实施第一步先把新端点、字段、错误码写进契约，再写代码。
`AGENTS.md`：只在设计/约定变化时同步（科目身份、迁移版本、组卷规则）。

