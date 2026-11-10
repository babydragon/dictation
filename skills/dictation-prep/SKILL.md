---
name: dictation-prep
description: 把老师消息、课本照片、杂乱词表整理成 dictation 项目可用的课时，并通过 dictation 服务的 HTTP API 导入词表、触发音频生成；也可查询听写结果、提取错词做记忆分析、清理 TTS 缓存。用于“准备听写”“整理词表”“生成听写音频”“分析错词”“看听写结果”“清理缓存”等场景。
---

# 听写词表准备（HTTP API 版）

你的任务不是教孩子，而是把家长手里的**原始材料**变成 `dictation` 能吃的
**纯文本词表 + 课时设置**，然后通过 `serve` 的 HTTP API 导入并生成。

> **不要执行 CLI，不要用文件工具读写主机路径。**
> agent 通常在沙箱里，文件白名单很窄；`/usr/local/bin` 的软链接也会被
> `realpath` 解析成 `/opt/dictation/...` 后被拒绝。所有操作只走 HTTP，
> 落盘由服务进程负责。

## 服务地址与鉴权

- 默认 `http://127.0.0.1:8080`；服务在另一台机器时问家长要局域网地址。
- 可用 `DICTATION_API_URL` 覆盖（含协议和端口，不带结尾 `/`）。
- 服务端若设置了 `DICTATION_API_TOKEN`，写操作需要请求头
  `X-Dictation-Token: <token>`。**只在本次会话的内存里用**：
  不要写进文件、不要回显到聊天、不要提交到仓库。

## 输出方式

三步：导入 → 生成 → 轮询。

```bash
API="${DICTATION_API_URL:-http://127.0.0.1:8080}"
AUTH=()
[ -n "${DICTATION_API_TOKEN:-}" ] && AUTH=(-H "X-Dictation-Token: $DICTATION_API_TOKEN")

# 1. 导入：201 新建 / 200 覆盖 / 409 已存在
curl -sS -X POST "$API/api/lessons" "${AUTH[@]}" \
  -H 'content-type: application/json' -d @payload.json

# 2. 生成：202 返回 job_id；已是最新则 {"status":"done","skipped":true}
curl -sS -X POST "$API/api/lessons/第5课/generate" "${AUTH[@]}" \
  -H 'content-type: application/json' -d '{"full_track": true}'

# 3. 轮询：queued → running → done/error
curl -sS "$API/api/jobs/<job_id>"
```

`payload.json`：

```json
{
  "title": "第5课 词语听写",
  "subject": "chinese",
  "grade": 3,
  "term": 1,
  "dir": "第5课",
  "text": "摇晃\n粗壮\n毕竟\n",
  "toml": "rate = 0.9\ngap = \"6s\"\nrepeat = 2\n",
  "force": false
}
```

- `subject`：**必填**，`chinese`（语文）或 `english`（英语）。缺了会 400，**没有默认值**；
  不确定就问家长，不要猜。
- `grade`（1–6）、`term`（1=上 / 2=下）：可选，用于后续按年级筛选复习。
- `text`：**一行一个词**，保持老师给的顺序；不要加标题行（服务端会写 `# title`）。
- `toml`：可选；不写则用默认设置。字符串里的换行用 `\n`。
- `dir`：可选；不给则从 `title` 生成安全目录名。
- `force`：默认 `false`；已存在返回 `409 {"error":"课时已存在","dir":"..."}`，
  确认要覆盖后再设 `true`。
- 成功返回：

```json
{ "created": true, "dir": "第5课", "title": "第5课 词语听写", "item_count": 20 }
```

响应里**没有主机绝对路径**，你也不需要知道它在 `/srv` 还是 `/var/lib`。

## 硬规则

- **不造词、不改词、不扩写。** 只做格式归一化。
- **保持顺序。** 老师给的顺序通常就是听写顺序。
- 去掉序号、项目符号、前后引号、多余空白；中文标点保留在句子里。
- 空行忽略；重复词只保留第一次，并在回复里指出“重复：xxx”。
- 英文单词/短语保持原样；不要把英文翻译成中文。
- 不要自动加拼音；只有家长明确要求时才生成答案纸。
- 不要自动加 `rate=` / `gap=`；只有明确需要放慢/加长时才加。
- **不要绕过 API 写文件**；不要尝试读取主机路径来“确认一下”。
- **先 import，后 generate。** import 只写词表/设置，不调 TTS；generate 才花钱。

## 归一化规则

| 原始 | 输出 |
|---|---|
| `1. 摇晃` | `摇晃` |
| `（2）粗壮` | `粗壮` |
| `- 毕竟` | `毕竟` |
| `3、旅行` | `旅行` |
| `“准备”` | `准备` |
| `apple,` | `apple`（去掉英文逗号） |
| `第5课 词语听写` | 作为 payload 的 `title`，不写进 `text` |

## 课时设置

默认值（不要问家长，除非明显不合适）：

```toml
title = "<课名>"
rate = 0.9
gap = "6s"
repeat = 2
repeat_gap = "1.2s"
lead_in = "3s"
announce_index = false
intro = "请准备好纸和笔，听写马上开始。"
intro_gap = "1.5s"
outro = "听写结束，请检查一遍。"
review_cap = 8
```

- 默认不逐题播报“第 N 个”。
- 开场白/结束语由程序按内容语言自动选择（全英文 → 英文，否则中文）；
  不要自己往 `text` 里加题号或结束语。
- **不要把 `DICTATION_PROVIDER` 写进 `lesson.toml`**；provider 由服务端配置
  （`/etc/dictation/env`）或 generate 请求的 `provider` 字段决定。

需要调整的典型情况：

- 词很长/句子：`gap = "10s"`，或给单条加 `gap=12s`。
- 孩子觉得太快：`rate = 0.8`。
- 只读一遍：`repeat = 1`。
- 英文为主：在 `lesson.toml` 里设 `voice_en`，不要把中文 voice 用于英文。
- 想改开场白/结束语：在 `toml` 里设 `intro` / `outro`；空串关闭。

## 工作流

1. 读取原始材料，按规则生成 `text` 和可选的 `toml` 字符串。
2. 可选：先确认是否已有同名课时：
   ```bash
   curl -sS "$API/api/lessons"     # { "lessons": [ { "id", "dir", "title", "item_count", "total_ms" } ] }
   ```
   `id` 用于调 API，`dir` 是相对目录名；判断是否重复用 `title`/`dir`，不要猜 id。
3. 导入：
   ```bash
   curl -sS -X POST "$API/api/lessons" "${AUTH[@]}" \
     -H 'content-type: application/json' -d @payload.json
   ```
   例：
   ```bash
   curl -sS -X POST "$API/api/lessons" \
     -H 'content-type: application/json' \
     -d '{"title":"第5课","text":"摇晃\n粗壮\n","toml":"rate = 0.9\ngap = \"6s\"\n"}'
   ```
4. 把导入结果用 3–5 行回报给家长：
   - 目录名 `dir`（只说名字，不用说绝对路径）
   - 词数 `item_count`
   - 是否有重复/超长/疑似多音字
   - 本次 `rate` / `gap` / `repeat`
5. **等待家长确认。**
6. 确认后生成：
   ```bash
   curl -sS -X POST "$API/api/lessons/第5课/generate" "${AUTH[@]}" \
     -H 'content-type: application/json' -d '{"full_track": true}'
   ```
   - 返回 `202 {"job_id":"...","status":"queued"}` → 每 3–5 秒轮询
     `GET /api/jobs/<job_id>`，直到 `done` 或 `error`。
   - 返回 `{"status":"done","skipped":true}` → 已是最新，不用重复生成。
   - 返回 `503` → 服务端没配 TTS 密钥。**不要索要 key**，告诉家长去
     `/etc/dictation/env` 配置后重启服务。
   - 想只验证流程：请求里带 `"provider": "mock"`，并提醒家长这不是最终音频。
7. 回报产物：`total_ms`、`tts_calls`、`cache_hits`；
   提醒家长前端刷新即可看到新课。

如果导入报“课时已存在”（`409`）：

- 默认不要覆盖；问家长是否要更新。
- 确认更新后，重新导入并在 payload 里设 `"force": true`。
- 覆盖后必须重新 generate，否则音频还是旧的。

## 更新已有课时

```bash
curl -sS "$API/api/lessons"                                  # 找 title/dir
curl -sS -X POST "$API/api/lessons" "${AUTH[@]}" ... -d @payload.json   # force=true
curl -sS -X POST "$API/api/lessons/第5课/generate" "${AUTH[@]}" -d '{}'
```

不要用文件工具直接改 `lesson.txt`；所有修改都走导入 API。

## 在沙箱 / 受限环境里运行

- 只需要能访问 `http://<主机>:8080`，**不需要**挂载 `/opt/dictation`、
  不需要 `dictation` 在 PATH、不需要读 `/etc/dictation/config.toml`。
- 密钥由服务进程持有；你这一侧不需要、也不应该拿到 TTS key。
- 如果连 HTTP 都不通：停止，告诉家长需要放行服务地址或改用本机 CLI；
  不要猜路径、不要用文件工具绕过。
- CLI（`dictation lesson import` / `lesson gen`）是主机侧运维入口，
  只在家长明确要求本机操作时使用。

## 复习组卷 / 加练（agent 挑词）

家长说“今天没有新词，帮我挑一些复习”“错词太多，帮我排个顺序”时，走 HTTP，
**不要自己猜词，也不要把词写回课时**。

### 1. 拉取待复习池

```bash
curl -sS "$API/api/review/queue?subject=chinese&limit=200"
```

- `subject` **必填**；`limit` 默认 100、最大 200；可选 `grade`/`term`。
- `total` 是范围内总量，`words` 已按「错词优先 → 最久未复习」排序；
  可以直接取前 N 个，也可以按你的教学判断重排。
- 只返回有音频、可播放的词。

### 2. 创建临时复习课时

```bash
curl -sS -X POST "$API/api/sessions/custom" "${AUTH[@]}" \
  -H 'content-type: application/json' -d '{
    "subject": "chinese",
    "title": "第一单元综合复习",
    "grade": 3,
    "term": 1,
    "items": ["粗壮", "摇晃", "旅行"]
  }'
```

- `items` 的顺序就是播放顺序；
- **最多 60 个词**。超过会返回
  `{"error":"复习词过多（最多 60 个，本次 N 个）","limit":60,"got":N}`，
  必须裁剪到 60 以内，或请家长分成两次复习；**不要原样重试**。
- 未知词 / 缺音频会分别返回 `unknown` / `unplayable` 列表，按列表修正后再试。
- 你只决定「选哪些词、什么顺序」，**不参与判分和排期**；对错与下次复习时间由
  应用批改后统一计算。

### 3. 把链接给家长

返回的 `session_id` 拼成 `$API/?s=<session_id>`，家长在手机上打开后点卡片开始。

### 直接加练

不需要挑词时，直接按科目开一个复习会话：

```bash
curl -sS -X POST "$API/api/sessions/review" "${AUTH[@]}" \
  -H 'content-type: application/json' -d '{"subject":"chinese","limit":20}'
```

`limit` 默认 20、最大 40。

## 查询听写结果 / 分析错词

> 走 HTTP，**不需要读 SQLite**。GET 端点不受 `DICTATION_API_TOKEN` 保护，
> 请求时不需要带鉴权头。主机侧 `dictation result ... --json` 输出与这些端点
> 逐字段相同，但 agent 应优先用 HTTP。

家长说“看下这次听写”“分析一下错词”“为什么这些词老错”时：

```bash
curl -sS "$API/api/results?limit=10"            # 最近已提交检查的听写
curl -sS "$API/api/results/latest"              # 最近一次完整结果
curl -sS "$API/api/results/<session_id>"        # 指定 session
curl -sS "$API/api/results/latest/wrong"        # 只取错词（推荐给大模型）
curl -sS "$API/api/results/<session_id>/wrong"
```

- 默认只返回已提交检查的 session；家长要“还没提交的那次”时加 `all=true`。
- `limit` 必须是 1–100；越界返回 400，**不要**自己重试同一个非法值。
- `404 {"error":"session 不存在"}` /
  `{"error":"还没有已提交检查的听写记录"}`：直接转述，不要猜 id 或反复重试。
- `409 {"error":"本次听写还没提交检查"}`：这次听写没有提交检查，没有对错可分析。
- `session.id` 是完整 id，从列表取；**不要**用 8 位短 id 拼 URL。

`wrong_words` 里每个错词包含：

- `text` / `lang`
- `origin_lesson`（来自哪一课；自动混入的复习词会指回原课）
- `position` / `review`
- `lapses` / `interval_days` / `due_at`
- `grade` / `source`

分析规则：

- **只使用接口返回的 `wrong_words`**，不要自己补词或猜错因。
- 可以按类型归纳：同音字、形近字、笔顺、前后鼻音、英文拼写/发音。
- 给出具体记忆钩子：拆字、组词、对比、造句、口诀；不要泛泛说“多读多写”。
- 可以建议下次复习顺序/分组，但**不要**直接修改课时或调用 gen；
  需要新增复习课时时，先按前面流程生成并让家长确认。
- 如果 `/api/results` 为空或 `/wrong` 返回 409，直接说明，不要编造结果。

## 清理 TTS 缓存

> **需要主机侧 CLI（写 `cache/`）。** 破坏性操作，默认 dry-run，确认后加 `--apply`。

缓存是内容哈希目录，删掉未引用的条目不会影响已生成课时的 `clips/`。
**默认是 dry-run，只有 `--apply` 才删。**

```bash
# 先看能清多少，不删除
dictation clean --unused --json

# 家长确认后真正删除
dictation clean --unused --apply --json

# 只清超过 30 天的（可选）
dictation clean --older-than 30d --json
dictation clean --older-than 30d --apply --json

# 最安全：既未引用、又超过 30 天
dictation clean --unused --older-than 30d --json
dictation clean --unused --older-than 30d --apply --json
```

- `--unused`：按课时 `clips/*-<hash8>.wav` 反查，删没有任何课时引用的条目。
- `--older-than`：按文件 mtime；`30d`/`12h`/`45m`/纯数字（天）。
- 两者同给：取交集（既未引用又超龄）。
- `.wav` / `.json` 成对删；孤儿文件也清；不跟随 symlink。
- 先跑 dry-run，把 `candidates` / `bytes` 报给家长；确认后再 `--apply`。
- 不要用 `rm` 或文件工具直接删缓存。

## 多音字与读法

只在**明显会读错**时加 `speak=`，不要逐词加：

```text
长大 | speak=长大的长
重要 | speak=重要的重
```

如果家长反馈某个词读错，优先建议用 `speak=` 换一个消歧短语；
不要自行改 display 文本。

## 英文与混排

- 纯英文词：正常一行一个，`lang` 自动检测。
- 中英混排的句子：保持整句一行，不要拆成中英两段；让 TTS 用双语 voice。
- 英文拼写听写：一行一个单词，`gap` 建议 8–10s。

## 不要做的事

- 不要用文件工具读写 `/srv`、`/opt` 或主机路径。
- 不要把整段老师消息原样写进 `text`。
- 不要生成 Markdown 表格或 YAML 作为 `text`。
- 不要在 import 后、家长确认前执行 `gen`。
- 不要删除家长给的词，即使你觉得重复或简单。
- 不要提交 git，不要写 API key 到文件。
