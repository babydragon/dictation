# 词汇表

> **本文件是本工程术语的唯一规范来源。**
> 设计文档、接口契约、代码标识符、CLI/HTTP 输出、UI 文案都用这里的词。
> 同一个概念只允许一个规范表述；旧称与近义词在 §7 明确标为「不要用」。
>
> 使用规则：
>
> 1. 新增概念、字段、端点、状态值时，**先在这里加一行**，再改代码与文档。
> 2. 一个概念已经有规范词时，不要另造近义词（例：不要写「课程」「加练」）。
> 3. 词表只定义「是什么」；规则细节放 `AGENTS.md` 与 `docs/*.md`，字段类型放
>    [`http-api.md`](http-api.md)。
> 4. 发现文档/代码用词与本表冲突时，以本表为准并修正冲突一方。
>
> 最后更新：2026-11-18

## 1. 内容生产（词表 → 课时）

| 术语 | 代码 / 字段 | 定义 | 不要用 |
|---|---|---|---|
| **词条** | `words` 表；身份 `(subject, text)` | 一个要被听写的单位。同一科目内跨课时共享复习状态；不同科目互不影响。「词」是它的日常简称 | 单词、词语（指数据实体时） |
| **显示文本** | `text` / `manifest.items[].text` | 检查页、答案纸、结果里显示的文本 | 原词（口语可） |
| **朗读文本** | 行内属性 `speak=` | 送给 TTS 的文本，用于多音字消歧；**不写入 manifest** | 发音文本 |
| **备注** | 行内属性 `note=` | 答案纸上的说明，不朗读 | — |
| **发音语言** | `lang`：`zh` / `en` / `mixed` | 选哪个音色读；与科目解耦 | 语言（易与科目混） |
| **词表** | `lesson.txt` | 家长输入的纯文本，一行一词，可带行内属性 | 词单、list |
| **行内属性** | `\|` 分隔的 `key=value` | 词级别的 `rate`/`gap`/`repeat`/`speak`/`lang`/`note` | — |
| **课时** | `lessons` 表；目录 `lessons/<dir>/` | 一次生成产出的播放单元（词表 + 清单 + 片段）。**也是复习范围的最小粒度** | 课程、课件 |
| **课时目录** | `dir`（相对目录名） | `lessons/` 下的目录名，不含主机绝对路径 | 路径 |
| **课时 id** | `lessons.id`（请求里叫 `lesson_id`） | `sha256(dir_name)` 前 12 位 hex；`/api/lessons` 与深链用它 | 课时编号、id 字符串 |
| **单元** | 未建模 | 教材单元。**当前没有字段**，一律用「课时」近似表达；不要为此新增概念 | 章节 |
| **科目** | `subject`：`chinese` / `english` | 一等隔离维度；写请求必填 | 学科、语言 |
| **年级 / 学期** | `grade` 1–6 / `term` 1–2 | 仅用于筛选与展示，不进词的身份 | 级别 |
| **课时清单** | `manifest.json`（v1） | 播放器的唯一输入；含默认参数、intro/outro、items、总时长 | manifest 文件（口语可） |
| **课时片段** | `lessons/<dir>/clips/*.wav` | 课时内实际播放的音频文件，命名 `<序号>-<hash8>.wav` | 音频、录音 |
| **缓存片段** | `cache/<sha256>.wav` + `.json` | 按内容哈希复用的合成结果；与课时片段是两份文件 | 缓存音频 |
| **缓存键** | `sha256(provider\|voice\|rate\|text\|pipeline_version)` | 命中即不调 TTS | 哈希 |
| **生成** | `dictation gen` / `POST /api/lessons/{id}/generate` | 调 TTS 产出片段与清单；`lesson import` **不生成** | 合成（口语可）、渲染 |
| **生成任务** | `job`；`status` | 后台生成单元：`queued` / `running` / `done` / `error` / `cancelled` | 任务队列 |
| **前导静音** | `lead_in` / `defaults.lead_in_ms` | 进入第一个词之前的静音 | 开头静音、前奏 |
| **开场白 / 结束语** | `intro` / `outro`（含 `intro_text`/`outro_text`） | 按内容语言自动选择并缓存的整句提示 | 片头、片尾 |
| **遍数** | `repeat` | 一个词连续读几遍 | 重复次数 |
| **间隔** | `gap` / `gap_ms` | 词后插入的静音，就是书写时间 | 停顿、等待 |
| **遍间间隔** | `repeat_gap` / `repeat_gap_ms` | 同一词两遍之间 | — |
| **完整音轨** | `full.wav` / `full.mp3`（`full_track`） | 按时间轴拼好的单文件，给普通播放器兜底 | 全长音频 |
| **预计时长** | `total_ms` | 时间轴总长，含静音 | 时长 |

## 2. 播放与练习

| 术语 | 代码 / 字段 | 定义 | 不要用 |
|---|---|---|---|
| **听写** | — | 孩子纸笔听写一次课时的完整过程（听、写、自检） | 测试、考试 |
| **会话** | `sessions` 表；`id` | 一次可播放、可提交的练习实例；条目在建会话时快照 | session（中文混用）、练习 |
| **课时会话** | `kind='lesson'` | 由 `POST /api/sessions` 创建；**唯一会自动混入**的会话 | 正常会话、新课会话 |
| **复习会话** | `kind='review'` | 由 `POST /api/sessions/review` 或 `/api/sessions/custom` 创建；**永不混入** | 练习会话 |
| **会话条目** | `session_items`；`position` 从 1 开始 | 会话里的一项，携带自己的片段地址与间隔参数 | item、题目 |
| **复习条目标记** | `is_review` | 该条目属于复习段或复习会话 | — |
| **自动混入** | `review_items_for_mix` | 在课时会话末尾、结束语之前追加待复习词；只发生在课时会话 | 附加、插入复习 |
| **混入上限** | `review_cap` | 一次混入的上限，默认 8，最大 15，`0` 关闭 | 复习数量 |
| **复习预算** | `REVIEW_BUDGET_MS` = 150s | 混入复习段的音频时长护栏，超出则少取 | — |
| **洗牌** | `shuffle` / `shuffle_review_items` | 按 `sha256(session_seed + text)` 排序；同会话稳定、不同会话不同 | 随机（口语可） |
| **深链** | `?s=<session_id>` | PWA 直接打开某个已创建会话 | 分享链接（F8 另有含义） |
| **断点续听** | `localStorage` 键值 | 首页「继续上次听写」，7 天过期 | 续播 |
| **自检** | 「检查页」+ `POST /api/sessions/{id}/check` | 孩子自己标错；默认全对，只标错 | 批改（另有所指） |
| **尝试** | `attempts`；`grade`=`good`/`again`；`source`=`self` | 一个词一次对错的记录 | 成绩、答题 |
| **结果** | `/api/results`、`result list/show` | 已完成会话的逐项对错视图（只读） | 报告 |
| **错词导出** | `result wrong`、`/api/results/{id}/wrong` | 只导出错词 + 来源课 + 调度字段，给模型分析 | 错题本（未实现） |

## 3. 复习与记忆曲线

| 术语 | 代码 / 字段 | 定义 | 不要用 |
|---|---|---|---|
| **复习状态** | `review_state` | 每个词一行的调度状态：`due_at`、`interval_days`、`lapses`、`last_grade` | 记忆状态 |
| **到期** | `due_at <= now` | 到了该复习的时间 | 过期 |
| **错词** | `last_grade='again'` | 最近一次答错、尚未纠正 | 错题 |
| **待复习** | `last_grade='again' OR due_at <= now` | 首页「错词复习 N」与组卷的统一口径 | 待复习池（口语可） |
| **复习间隔** | `interval_days` | 当前阶梯：1 / 2 / 4 / 7 / 15 / 30 天，30 天封顶 | 周期 |
| **回退次数** | `lapses` | 答错并重置的次数；比「错过一次」更接近「真不会」 | 失败次数 |
| **提前复习** | — | 未到期、也非错词的词被拿来练；答对**不改调度**，答错照常回退 | 超前复习、预习 |
| **课内复习** | `POST /api/sessions/review` + `lesson_id` | 只取**本课**待复习词；孩子端唯一的课时级复习入口 | 单元复习、课程复习 |
| **错词复习** | `POST /api/sessions/review`（`limit` 1–40） | 首页按科目练全部待复习词（`title='错词复习'`） | **加练**（旧称，不要再用） |
| **范围复习** | `GET /api/words` + `POST /api/sessions/custom` | 跨课时按范围挑词组卷；只由 agent / 家长端发起 | **AI 组卷**（旧称）、单元复习 |
| **整课重排** | 范围复习的一种（`items` = 本课全部词） | 顺序：错词分桶 → 桶内洗牌；仅 agent / 家长端 | 整课复习、重听（那是「再听一遍」） |
| **组卷** | `review_items_for_mix` / `create_custom_session` | 决定一次会话包含哪些词、什么顺序 | 排卷 |
| **积压** | `GET /api/review/summary` | 某科目待复习词的合计与逾期天数分布，用来判断是否需要主动复习 | 库存、欠账 |
| **删除课时** | `lesson delete` / `POST /api/lessons/{id}/delete` | 删除课时目录与 `lessons` 行；**默认 dry-run**；保留历史，词音源重指向其他课时 | 清理、移除 |
| **重复课时** | `GET /api/lessons/duplicates` | 同科目同标题的多份课时（通常是重新导入留下的旧副本） | 冲突课时 |
| **重新生成** | `POST /api/lessons/{id}/regenerate-word` | 删除某个词的全部缓存样本后强制重渲染；其它词命中缓存，只花 1 次 TTS；片段文件名不变 | 重掷（口语可） |
| **听写节奏** | `localStorage["dictation.timing.v1"]` | 孩子端本地设置，只改**词后等待**的秒数（换算 `gap_ms = 秒×1000 − 80`）；纯客户端，静态发布同样可用 | 语速（另有所指） |
| **掌握度统计** | `attempts`/`wrong`/`correct`/`correct_streak`/`wrong_recent` | 词的客观练习数据；**服务端不判定「是否掌握」** | 掌握度（不要当结论用） |

## 4. 接口、配置与实现约定

| 术语 | 代码 / 字段 | 定义 | 不要用 |
|---|---|---|---|
| **契约** | `docs/http-api.md` | HTTP 字段、状态码、错误格式的唯一来源 | 接口文档（口语可） |
| **共用实现** | `lesson::import` / `lesson::list` / `result_view` | CLI 与 HTTP 同一能力必须调用同一份代码 | 双写 |
| **运行时能力** | — | 允许 CLI/HTTP 不一致的部分：生成任务队列、鉴权、媒体、会话与检查 | 特例 |
| **服务端生成** | `POST /api/lessons/{id}/generate` | agent 不持有 TTS 密钥，合成在 `serve` 进程内完成 | 后台生成（口语可） |
| **可播放** | `has_clip` / `unplayable` | 词的片段存在，能进会话；缺失的词不能组卷 | 有音频 |
| **路径根** | `config.toml` → `paths.base` | `lessons/`、`data/`、`cache/` 的父目录 | 数据目录（口语可） |
| **数据库文件** | `paths.data` → `dictation.db` | SQLite；由 `serve` 独占写入 | 数据库（口语可） |
| **schema 版本** | `PRAGMA user_version` | 当前 v4；库版本高于程序则拒绝启动 | 迁移号 |

## 5. 用户与角色

| 术语 | 定义 | 不要用 |
|---|---|---|
| **家长** | 准备词表、调参、打印、确认批改的人 | 用户、管理员 |
| **孩子** | 做听写与自检的人；孩子端只暴露最少操作 | 学生、儿童（文档内可） |
| **agent** | 通过 HTTP 操作系统的助手；不 exec CLI、不读主机路径 | 机器人、AI（笼统） |
| **家长模式** | 计划中的 PIN 保护界面（F5），放范围/数量/整课重排等设置 | 管理模式 |

## 6. 状态值与枚举（写死，不要造新值）

| 枚举 | 允许值 |
|---|---|
| `subject` | `chinese` / `english` |
| `lang` | `zh` / `en` / `mixed` |
| `sessions.kind` | `lesson` / `review`（计划中：`imported`） |
| `attempts.grade` | `good` / `again` |
| `attempts.source` | `self`（计划中：`import`） |
| `review_state.last_grade` | `good` / `again` |
| `job.status` | `queued` / `running` / `done` / `error` / `cancelled` |
| 照片批改 `verdict` | `correct` / `wrong` / `missing` / `unclear`（F6） |

## 7. 易混词与旧称（不要用）

| 不要用 | 用 | 原因 |
|---|---|---|
| 课程、课件 | **课时** | 「课程」曾被用于指一个课时，与教材课程混淆 |
| 加练 | **错词复习** | 旧称；`POST /api/sessions/review` 的科目级入口 |
| AI 组卷 | **范围复习** | 旧称；强调「按范围挑词」，不是「让 AI 出题」 |
| 单元、章节（指复习范围时） | **课时** | 教材单元未建模，不要暗示有 `unit` 字段 |
| 单词、词语（指数据实体时） | **词条** | 代码与数据里是 `words` 表的一行 |
| 音频（指单条文件时） | **片段**；拼接结果叫 **完整音轨** | 两者生命周期不同，不能混称 |
| 缓存文件 | **缓存片段** vs **课时片段** | 同名 `.wav` 是两份文件，删除策略不同 |
| 复习计划、`review_plan` | **范围复习**（范围是请求参数） | 不引入计划实体；见 `review-scheduling.md` §6.6 |
| 掌握 / 已掌握 | **掌握度统计**（只给数字） | 服务端不判定掌握，避免与调度结论混淆 |
| 练习 session | **复习会话** | 统一用「会话」 |
| 错题本 | **错词导出**（已有）/ 答案纸（F5） | 「错题本」尚未定义 |
