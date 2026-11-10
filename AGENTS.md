# Dictation — 儿童听写生成与练习系统

> **本文件是稳定设计与约定的唯一来源。**
> 进度、验收、工作日志在 [`TODO.md`](TODO.md)；新 session 先读本文件，再读
> `TODO.md` 的「当前状态 / 下一步」。
>
> 设计版本：1.0。**不要为了记录进度修改本文件**；只有设计/约定变化时才改。

---

## 1. 项目目标

把一份**纯文本词表**变成可精确控制节奏的听写音频/播放会话：

- 每个词单独 TTS，词与词之间插入**毫秒级精确**的等待静音。
- 支持中文、英文、中英混合。
- 支持课时级、单词级语速；支持重复遍数；开场白/结束语按语言自动选择。
- 孩子用纸笔听写；结束后自检；错词进入复习并自动混入后续课时。
- 可扩展拍照批改、家长模式、家庭服务器部署。

### 非目标

- 不做通用音乐播放器。
- 不做在线教育平台、账号体系、多租户。
- 不追求 TTS 的“朗诵级”情感表现；清晰、稳定、可复现优先。
- 不做实时流式 TTS；批量预生成即可。

---

## 2. 用户与场景

- **家长**：技术型，负责准备词表、偶尔调参、打印答案纸。
- **孩子**：小学三年级，中文为主；纸笔听写；设备为手机/平板（iOS/Android 浏览器）。
- **家庭服务器**：Linux 主机（x86_64 或 riscv64），至少 2 核 CPU、
  1GB RAM（建议 2GB），8GB 可用磁盘（建议 20GB+；SSD/NVMe 更好），
  有线或无线局域网；预编译二进制需要 glibc ≥2.30。
  本地 TTS 属于可选能力，建议 4GB+ RAM 和额外 2GB 磁盘。
  课件与缓存按每课约 3–8MB 估算，100 课建议预留 1–2GB。
- **典型规模**：一次语文听写 20–40 个词；可能追加按比例（25%，上限 8）的复习词。
- **网络**：家里 WiFi；允许云端 TTS 和云端多模态批改。

### 交互原则

- 孩子侧尽量少操作：开始/暂停、重听当前词、完成、检查。
- 不显示正在听写的词；只显示进度 `3 / 20`。
- 大按钮、全屏、防误触、屏幕常亮。
- 家长不要求编辑 markdown；用纯文本 + agent skill 归一化。

---

## 3. 硬性需求

| ID | 需求 | 验收 |
|---|---|---|
| R1 | 输入纯文本，一行一词 | 编号、项目符号、空行、中文标点均可容错 |
| R2 | 逐条 TTS，不依赖 SSML 停顿 | 词间静音由本地拼接，误差 < 20ms |
| R3 | 中/英/混合 | 按脚本检测选 voice；混合可用双语 voice |
| R4 | 语速可调 | 课时级、单词级、播放时全局；0.5–2.0 |
| R5 | 等待间隔可调 | 课时级、单词级 |
| R6 | 重复遍数可调 | 默认 2 遍，遍间 1.2s |
| R7 | 纸笔听写 | A4 一列 20 题，带题号（后续实现） |
| R8 | 听后自检 | 显示所有词；只标错，未标默认正确 |
| R9 | 拍照批改 | 多模态逐题转写 + 比对 + 置信度 |
| R10 | 记忆曲线 | 错词/到期词进入复习；自动混入后续课时 |
| R11 | 家庭部署 | SBC 上跑服务；手机/平板浏览器访问 |
| R12 | TTS 可切换 | 至少豆包、百炼 Qwen 两个 provider |
| R13 | 离线兜底 | SBC 可选本地 TTS |
| R14 | 成本可控 | 内容哈希缓存；重复生成不重复计费 |

---

## 4. 关键设计决策

### D1. 逐条合成 + 本地拼接

TTS 的 SSML `<break>`、prompt 停顿都不可靠，流式 chunk 边界也不等于音频时间轴。
**词/句是合成单元，停顿是应用层的事。** 这样间隔精确、可复现、可单条重听、可缓存。

### D2. 每条音频裁首尾静音

TTS 输出首尾通常有 100–300ms 不等的静音；不裁剪会让 5s 听起来像 4.7s 或 5.4s。
流程：VAD → 保留 20ms 前导 / 60ms 尾音 → 5ms 淡入淡出 → RMS 均衡。

### D3. 片段缓存按内容哈希

`sha256(provider|voice|rate|text|pipeline_version)`。
改间隔、改顺序、重跑都不再计费。

### D4. manifest + 播放器动态排时间轴

播放器不重新合成 MP3，而是按 manifest 里的 `clips + gap + repeat` 实时排程。
同时导出 `full.mp3` 作为普通播放器兜底。

### D5. 批改在服务端完成

`POST /api/...` 内部调多模态模型，结构化返回并落库；agent 不参与每次判卷。
理由：可复现、可审计、可人工纠正。

### D6. AI 批改只做预填

默认自检；拍照批改为增强。低置信度、模糊、题号缺失一律转人工。
AI 结果永远不是最终裁决。

### D7. SBC 是常驻服务端，不承担实时 TTS

云端 TTS 为主；SBC 本地 TTS 只用于备课/断网兜底。
SBC 同时跑服务 + SQLite 没问题；本地 TTS 只在生成时占用 CPU。

### D8. 输入以纯文本为主，agent skill 归一化

家长只写一行一词；`lesson.toml` 和行内属性由 agent 生成。
程序解析器要容错，不能要求家长理解 YAML/markdown。

### D9. 自检只标错

检查页显示所有词；默认全部正确，只有孩子点过的才算错。
理由：三年级孩子，交互越少越好；减少误操作和检查时间。

### D10. 错词复习 + 自动混入

- 错词/到期词进入 `review_state`；首页有“错词复习”入口。
- 下一课自动混入待复习词（25% × 新词数，保底 3、上限 `review_cap`）：
  追加在本课词之后、outro 之前。
- **不加额外语音提示**；检查页用「复习」标签区分。
- 复习标对后，首页入口和后续混入消失。

### D11. 中英文响度按 RMS 均衡

纯峰值归一会让中文 Vivi 比英文 Stokie 响约 4 dB。
统一按有效语音 RMS 归一到 **-19 dBFS**，峰值上限 **-1.5 dBFS**。

### D12. 开场白/结束语按语言自动选择并缓存

中文内容用中文，全英文内容用英文；相同文本 + 音色 + 语速只生成一次。
开场白在 lead-in 后；结束语在最后一个 item 的 gap 之后。

### D13. agent 通过 HTTP 写入，不 exec、不碰主机目录

agent 通常在沙箱中，文件系统白名单很窄，软链接也会被 `realpath` 解析后拒绝。
因此 agent **不执行 CLI、不读任何路径**，只调 `serve` 的 HTTP API：

- `POST /api/lessons` 导入词表；
- `POST /api/lessons/{id}/generate` 触发合成；
- `GET /api/jobs/{id}` 轮询进度。

服务进程按 `/etc/dictation/config.toml` 解析路径并落盘。agent 既不需要知道
`/srv` 或 `/var/lib` 的实际位置，也不需要 `dictation` 在 PATH 里。
CLI（`lesson import` / `lesson gen`）保留为**运维与本地调试**路径。

写操作可用 `DICTATION_API_TOKEN` 保护；未设置时按家庭局域网开放。

### D13b. TTS 密钥属于应用，不属于 agent

密钥只放 `/etc/dictation/env`（root:root 0600），由 `serve` 读取；
合成在服务侧完成，所以 agent 和你自己的 shell 都不需要持有 key。
需要进一步收紧时，systemd 用 `LoadCredential` 注入：代码优先读
`$CREDENTIALS_DIRECTORY/<NAME>`，回退环境变量，密钥因此不进 `environ`、
也不会被子进程（如 `ffmpeg`）继承。

### D14. 结果查询与错词导出

听写结果从 SQLite 只读查询，不另存导出文件：

- `dictation result list [--json]`：最近的 session；
- `dictation result show <id>|--latest [--json]`：整次逐项对错；
- `dictation result wrong <id>|--latest [--json]`：只导出错词 + 来源课 +
  `lapses/interval_days/due_at`，供大模型做记忆分析。

时间输出 RFC3339；`--json` 错误走 stderr + exit 1。CLI 只读，不修改 `data/`。

### D15. 科目隔离与 schema 版本

- 科目是一等维度：`subject ∈ {chinese, english}`，写请求**必填**；
  `grade`（1–6）/`term`（1/2）是可选的筛选/展示元数据。
- 词的身份是 `(subject, text)`，不是全局 `text`；语文和英语的同形词互不影响。
- 复习、积压、组卷都按科目隔离，一个 session 不混科目。
- SQLite 用 `PRAGMA user_version` 管理 schema（当前 v3）：
  - 启动时自动迁移；迁移前 `VACUUM INTO` 备份，保留最近 3 份；
  - 库版本高于程序 → 拒绝启动；
  - 迁移只追加、不修改；新增列必须有默认值或可空。
- 详见 [`docs/review-scheduling.md`](docs/review-scheduling.md)。

---

## 5. 输入格式

### 5.1 `lesson.txt`（家长主入口）

```text
# 第5课 词语听写
摇晃
粗壮
毕竟
apple
library
```

解析规则：

- 忽略 BOM、空行、以 `#` 开头的注释/标题。
- 去掉行首 `-`、`*`、`1.`、`1、`、`1)`、`（1）` 等编号。
- 去掉首尾空白和全角空格。
- 行内属性用 `|` 分隔，仅当后面是 `key=value` 时生效：
  `摇晃 | rate=0.8 gap=8s`
- 支持转义 `\|`。
- 重复词保留第一次出现的位置，并 warn。
- 单条长度上限 200 字符。

### 5.2 `lesson.toml`（可选，同目录）

```toml
title = "第5课"
subject = "chinese"            # 必填：chinese / english
grade = 3                      # 可选 1–6，仅用于筛选/展示
term = 1                       # 可选 1=上 / 2=下
rate = 0.9
gap = "6s"
repeat = 2
repeat_gap = "1.2s"
lead_in = "3s"
announce_index = false          # 默认不逐题播报“第 N 个”
intro = "请准备好纸和笔，听写马上开始。"  # 可选；空串关闭
intro_gap = "1.5s"
outro = "听写结束，请检查一遍。"          # 可选；空串关闭
voice_zh = "Cherry"
voice_en = "Aiden"
voice_mixed = "Cherry"
# cue = "beep"                  # 保留字段；当前未实现，默认 none
review_cap = 8
```

时间字段接受 `6000`、`6000ms`、`6s`、`1.2s`。
`rate` 0.5–2.0；`gap` 0–120s；`repeat` 1–10。

### 5.3 行内属性

| 属性 | 示例 | 说明 |
|---|---|---|
| `rate` | `rate=0.8` | 单词语速 |
| `gap` | `gap=8s` | 该词后的等待 |
| `repeat` | `repeat=1` | 该词读几遍 |
| `speak` | `speak=长大的长` | 送给 TTS 的文本，display 仍是原词 |
| `lang` | `lang=en` | 强制语言 |
| `note` | `note=形容词` | 答案纸备注，不朗读 |

### 5.4 语言检测

- 含 Han 且无拉丁 → `zh`
- 含拉丁且无 Han → `en`
- 两者都有 → `mixed`
- 数字/符号跟随课时主语言；必要时 `speak` 覆盖。

### 5.5 多音字

优先用 `speak=` 消歧（如 `长大 | speak=长大的长`）。
仍读错时允许 `overrides/<sha>.wav` 手工替换（后续实现）。

---

## 6. 音频流水线

### 6.1 统一格式

- PCM signed 16-bit little-endian，单声道，**24000 Hz**。
- 所有 provider 输出先解码/重采样到该格式；当前用内置线性重采样，
  需要更高质量再引入 `rubato`。
- 内部 DSP 在 `Vec<i16>` 上完成，不依赖 ffmpeg。

### 6.2 每条片段的处理

1. 解码 → mono 24k。
2. 去 DC（减均值）。
3. VAD：10ms 帧，RMS 阈值 -45 dBFS；找首尾 voiced 帧。
4. 保留 20ms 前导 + 60ms 尾音；最短 100ms。
5. 5ms 余弦淡入/淡出。
6. **有效语音 RMS 归一到 -19 dBFS**；峰值超过 -1.5 dBFS 时整体回缩。
7. 写 `clips/<hash>.wav`，记录 `duration_ms`。

### 6.3 时间轴

```
lead_in
  → [intro?]（中文/英文按内容自动）
  → intro_gap
  → item 1: clip × repeat（遍间 repeat_gap） + gap
  → item 2: ...
  → ...
  → [outro?]（最后一个 item 的 gap 之后）
```

- 静音用**零采样**精确写入，不靠 ffmpeg `adelay`。
- `announce_index` 默认关闭；开启才生成“第 N 个”。
- 开场白/结束语相同文本+音色+语速缓存复用。

### 6.4 gap 语义

`gap_ms` 是**插入的静音**，按采样点精确写入。
片段保留 20ms 前导 + 60ms 尾音，所以**可听静音 ≈ gap + 80ms**。
例如 `gap=6000` 时，两个有声区间之间约 6080ms。播放器倒计时从片段结束开始，
与 manifest 语义一致。

### 6.5 语速

- 豆包 V3：`audio_params.speech_rate` int，0=正常，100=2.0x，-50=0.5x。
- Qwen `qwen3-tts-flash` 不支持 rate：正常速合成后做不变调时长伸缩
  （ffmpeg `atempo` 或纯 Rust WSOLA）。
- 缓存键包含最终 `rate`。

### 6.6 MP3 导出

- 先写临时 WAV，再调用 `ffmpeg -codec:a libmp3lame -b:a 96k`。
- ffmpeg 不存在时保留 WAV 并 warn，不阻塞主流程。

### 6.7 缓存

```
cache/
├── <sha256>.wav
└── <sha256>.json   # duration_ms, sample_rate, provider, voice, rate, text
```

- 命中直接复用，不调 TTS。
- `pipeline_version` 变化时整体失效。
- `dictation clean --unused` 按课时 clip 反查引用；默认 dry-run，`--apply` 才删；
  可叠加 `--older-than 30d` 取交集。

---

## 7. 产物与 manifest

```
lessons/第5课/
├── lesson.txt
├── lesson.toml          # 可选
├── manifest.json
├── clips/01-ab12cd.wav
├── full.wav             # 始终生成
├── full.mp3             # --full-track 且 ffmpeg 可用
└── answers.html         # 后续
```

`manifest.json`（v1）：

```json
{
  "version": 1,
  "title": "第5课",
  "generated_at": "2026-10-20T12:00:00+08:00",
  "defaults": {
    "gap_ms": 6000,
    "repeat": 2,
    "repeat_gap_ms": 1200,
    "lead_in_ms": 3000,
    "intro_gap_ms": 1500
  },
  "intro": { "path": "clips/intro-ab12cd.wav", "duration_ms": 3300 },
  "intro_text": "请准备好纸和笔，听写马上开始。",
  "outro": { "path": "clips/outro-ab12cd.wav", "duration_ms": 2600 },
  "outro_text": "听写结束，请检查一遍。",
  "items": [
    {
      "index": 1,
      "text": "摇晃",
      "lang": "zh",
      "clips": [{ "path": "clips/01-ab12cd.wav", "duration_ms": 1220 }],
      "repeat": 2,
      "repeat_gap_ms": 1200,
      "gap_ms": 6000
    }
  ],
  "full_track": "full.mp3",
  "total_ms": 570000
}
```

约定：

- `text` 是 display 文本；`speak` 不写进 manifest。
- `intro`/`outro` 可省略；`announce` 仅显式开启时出现。
- 播放器只读 manifest，不读 `lesson.txt`。
- `full_track` 指向实际存在的 `full.mp3` 或 `full.wav`。
- 复习词在会话层追加，不改原 lesson 的 manifest。

---

## 8. 播放器与自检

### 8.1 首页

- 课时卡片：标题、词数、预计时长。
- 有 `review_count > 0` 时显示“错词复习 (N)”卡片。
- 有 7 天内的未完成进度时显示“继续上次听写”卡片（标题 + `3 / 20`）；
  可“不继续”清除（二次确认）。
- 空状态提示用 `dictation gen` 生成课时。

### 8.2 播放页

- 时间轴：lead_in → intro → intro_gap → items → outro。
- 大号进度 `3 / 20`；不显示词。
- `暂停/继续`、`重听这个词`、`返回`（二次确认）。
- 完成后：`检查` / `再听一遍`（从头，同一 session）/ `返回首页`。
- 中途返回首页保留进度：`localStorage` 存 session manifest + 当前 item；
  从首页“继续上次”进入时跳过 lead-in/intro，直接从该 item 开始；
  完成/提交检查/清除/加载失败时清除；7 天过期。
- 音频开始必须由用户点击触发（iOS）。
- 全屏、Wake Lock、防误触。

### 8.3 检查页

- 显示本次所有词（含自动混入的复习词）。
- 每行：序号、词、`🔊` 单听、`标错` 开关。
- **默认全部正确；只有点过的才算错**；再点取消；标错行变红。
- 底部：已标错 N 个 + 完成检查 + 跳过。
- 提交所有 position；成功后显示 `X 对 / Y 错`。
- 复习词显示「复习」小标签；播放时不显示词。
- 未提交离开需确认；提交失败保留标记并可重试。

### 8.4 错词复习

- 首页“错词复习”入口：单独练全部待复习词（最多 20）。
- 待复习 = `last_grade='again' OR due_at <= now`。
- 复习 session 无 intro/outro，lead-in 3s。
- 标对后从入口和后续混入中消失。

---

## 9. 批改

### 9.1 自检（默认）

检查页只标错；结果写 `attempts`，`source=self`。

### 9.2 拍照 + 多模态（后续）

- 一张整页照片；光线均匀、正对、题号可见。
- Prompt 带期望词表与题号，要求逐题输出：

```json
{
  "items": [
    { "no": 1, "seen": "摇晃", "verdict": "correct", "wrong_chars": [], "confidence": 0.98 },
    { "no": 2, "seen": "粗状", "verdict": "wrong", "wrong_chars": ["状→壮"], "confidence": 0.93 },
    { "no": 3, "seen": "?", "verdict": "unclear", "wrong_chars": [], "confidence": 0.41 }
  ],
  "overall_confidence": 0.9,
  "notes": ""
}
```

- `verdict` 只允许 `correct | wrong | missing | unclear`。
- `confidence < 0.75` 或 `unclear` → 人工确认。
- 同音字错误记 `wrong_chars`。
- 模型：`deepseek-flash`（原生多模态）或 Qwen-VL；`Grader` trait 隔离。
- 图片默认本地存；上传云端有显式开关。
- AI 只预填，家长确认是最终裁决。

---

## 10. 复习与记忆曲线

概念模型：

- 词以 `(subject, text)` 去重，同科目内跨课时共享状态；科目之间严格隔离。
- `attempts` 记录每次对错；`review_state` 记录到期时间、间隔、lapses。
- 调度：错 → 1 天；对 → 1/2/4/7/15/30 天递增；30 天封顶。
- 待复习 = `last_grade='again' OR due_at <= now`；错词当天即可复习。
- 组卷：本课新词 + `min(review_cap, max(3, 25%×新词数))` 个**同科目**待复习词，
  追加在本课之后、outro 之前；
  跳过本课已有词；不加语音提示。
- 首页另有“错词复习”入口，可单独练；默认 20 个，可选 1–40，支持按年级/学期筛选。
- 后续可换 FSRS；接口保持不变。

**实际 SQLite schema 与 API 以 [`docs/http-api.md`](docs/http-api.md) 为准。**

---

## 11. 架构

```
家长：纯文本 / agent skill
        ↓
axum 服务（SBC / 本机）
  ├─ POST /api/lessons            导入词表（校验后写 lessons/）
  ├─ POST /api/lessons/{id}/generate → 后台 job
  ├─ GET  /api/jobs/{id}          轮询
  ├─ GET  /api/review/queue       待复习池（agent 挑词）
  ├─ POST /api/sessions/custom    临时复习课时（agent 给词表和顺序）
  ├─ lessons / sessions / check / review
  └─ media / static PWA
        ↓（生成由服务进程执行，持有 TTS 密钥）
lesson 解析 / plan
        ↓
TTS Provider（豆包/Qwen/mock） + 内容缓存
        ↓
裁剪 / RMS / 重采样 / 精确静音 / MP3
        ↓
clips + manifest + full.mp3
        ↓
PWA（手机/平板） / agent（只用 HTTP）
```

同一个进程既提供 API 也执行生成；生成串行（并发 1），避免打满小机器。
CLI 仍保留 `dictation gen` 作为运维/本地调试入口。

---

## 12. 技术栈与依赖约束

### 后端 / CLI（Rust 2021）

| 用途 | 选择 |
|---|---|
| CLI | `clap` 4 |
| 异步 | `tokio` |
| HTTP 客户端 | `reqwest`（rustls，避免 OpenSSL） |
| HTTP 服务 | `axum` 0.8 |
| 序列化 | `serde` / `serde_json` / `toml` |
| WAV | `hound` |
| 重采样 | 内置线性；后续可 `rubato` |
| 哈希 | `sha2` |
| 时间 | `time` |
| 日志 | `tracing` / `tracing-subscriber` |
| 错误 | `anyhow` + `thiserror` |
| SQLite | `rusqlite`（bundled） |
| MP3 | 外部 `ffmpeg` |
| ID | `uuid` |

### 前端

- 纯静态 HTML/CSS/vanilla JS，无构建步骤、无外部 CDN。
- 由 axum 直接托管；同源 `fetch`。
- PWA manifest 用于“添加到主屏幕”；v1 不加 Service Worker
  （iOS 局域网 HTTP 没有 SW）。
- 不用重型框架。

### 配置

默认 `/etc/dictation/config.toml`；`--config <path>` 或 `DICTATION_CONFIG`
可覆盖。配置不含密钥（密钥走环境变量）。

```toml
[paths]
base = "/srv/dictation"
lessons = "lessons"      # 相对 base；也可绝对
data = "data"            # DB = <data>/dictation.db
cache = "cache"
web = "/opt/dictation/web"

[server]
host = "0.0.0.0"
port = 8080
```

- `lessons`/`data`/`cache` 相对 `base`；`web` 默认相对 base，示例用绝对。
- `--config` 是 clap global arg，所有子命令可用。
- `dictation config show [--json]` 输出解析后的路径与来源。
- 安装脚本的 `--data-root` 可把 `base` 改为 `/var/lib/dictation` 等其他目录；
  默认 `/srv/dictation`。改目录时同步更新 managed unit 的 `ReadWritePaths`。

### 部署 / 交叉编译

- SBC：`riscv64gc-unknown-linux-gnu`，Linux（glibc ≥2.30）；systemd 常驻。
- 优先本地 `cargo zigbuild` 交叉编译。
- 目录分离：`/opt/dictation` 只放只读程序（二进制、`web/`、INSTALL）；
  `/srv/dictation/{lessons,data,cache}` 放可写数据。
  agent 以普通用户运行，加入 `dictation` 组，只写 `lessons/`/`cache`；
  `data/`（SQLite）由服务独占写入。
- **依赖约束：新增依赖优先纯 Rust；尽可能避免 `*-sys` crate**
  （如 `openssl-sys`、`libsqlite3-sys`），降低 riscv64/zigbuild 风险。
- 当前已知例外：`rusqlite(bundled)` 会引入 `libsqlite3-sys`。
  SBC 部署前必须用 zigbuild 验证；失败则评估 `redb` / JSON 等纯 Rust 存储。
- `reqwest` 用 rustls 避免 OpenSSL；`ring` 的 C/asm 在 riscv64 上需验证。
- `ffmpeg` 是外部命令，不是 crate 依赖。

### 发布

- 版本号：正式版 `vX.Y.Z`，测试版 `vX.Y.Z-rc.N`（如 `v0.1.0-rc.2`）；
  tag 必须与 `Cargo.toml` 一致，CI 会校验。
- CI 对 `-rc`/`-alpha`/`-beta` 自动加 `--prerelease`，其余为正式 release。
- GitHub Actions：测试 → 构建 x86_64 + riscv64 → 打包 tgz → 创建
  release/pre-release 并上传。
- 资产包含二进制、`web/`、`deploy/`、skill、`INSTALL.md`、`install.sh`、
  `SHA256SUMS`。
- `install.sh` 幂等：更新程序/前端/managed unit；保留已有 `config.toml`、
  env、`lessons/`、`data/`、`cache/`；需要 root；支持 `--dry-run`。

---

## 13. 目录结构

```
dictation/
├── AGENTS.md              # 本文件：稳定设计
├── TODO.md                # 进度/验收/日志
├── docs/
│   ├── http-api.md        # 服务端 HTTP API / SQLite / 一致性契约
│   ├── player.md          # 儿童端播放器与自检交互
│   └── review-scheduling.md # 科目隔离、复习组卷、AI 临时复习
├── deploy/
│   ├── dictation.service  # systemd unit
│   ├── config.example     # /etc/dictation/config.toml
│   └── env.example
├── INSTALL.md             # 安装/交叉编译/systemd
├── Cargo.toml
├── README.md
├── fixtures/
├── src/
│   ├── main.rs
│   ├── lib.rs
│   ├── config.rs
│   ├── model.rs
│   ├── cache.rs
│   ├── lesson/mod.rs
│   ├── lesson/import.rs    # CLI 与 HTTP 共用的导入/校验
│   ├── lesson/list.rs      # CLI 与 HTTP 共用的课时列表/排序/JSON
│   ├── plan.rs
│   ├── audio/mod.rs
│   ├── render.rs
│   ├── manifest.rs
│   ├── db.rs
│   ├── result_view.rs      # CLI 与 HTTP 共用的结果 JSON
│   ├── server/mod.rs
│   └── tts/{mod,doubao,qwen,mock}.rs
├── web/
│   ├── index.html
│   ├── style.css
│   ├── app.js
│   └── manifest.webmanifest
├── scripts/
├── skills/dictation-prep/SKILL.md
├── lessons/               # 运行时（gitignore）
├── data/                  # SQLite（gitignore）
├── cache/                 # 片段缓存（gitignore）
└── demo/                  # 试听样本（*.mp3 gitignore）
```

---

## 14. CLI 与配置

```bash
# 只算时长
dictation plan fixtures/lesson-20.txt
dictation plan fixtures/lesson-20.txt --json

# 合成（路径模式，人类使用）
dictation gen fixtures/lesson-20.txt --out lessons/第5课
dictation gen fixtures/lesson-20.txt --provider doubao
dictation gen fixtures/lesson-20.txt --provider qwen
dictation gen fixtures/lesson-20.txt --provider mock
dictation gen ... --full-track

# 配置化路径 / agent 导入
dictation --config /etc/dictation/config.toml config show --json
dictation lesson list --json
printf '%s' '{"title":"第5课","text":"摇晃\n粗壮\n"}' \
  | dictation lesson import --payload - --json
dictation lesson plan --title "第5课" --json
dictation lesson gen --title "第5课" --provider doubao --full-track

# 结果查询 / 错词导出
dictation result list --json
dictation result show --latest --json
dictation result wrong --latest --json

# 播放器 / 自检
dictation serve
dictation serve --config /etc/dictation/config.toml
dictation serve --dir lessons --web web --data data/dictation.db \
  --host 0.0.0.0 --port 8080

# 维护
dictation new "第5课"
dictation clean --unused --json            # dry-run
dictation clean --unused --apply --json    # 确认后
dictation clean --older-than 30d --json
```

`--config` 是全局参数；也可用 `DICTATION_CONFIG`。默认读
`/etc/dictation/config.toml`（存在时）。`lesson import` 只写词表/设置，
不调 TTS；`lesson gen` 才调 TTS。

环境变量：

```bash
# 百炼 Qwen-TTS
DASHSCOPE_API_KEY=sk-...
QWEN_TTS_MODEL=qwen3-tts-flash
QWEN_VOICE_ZH=Cherry
QWEN_VOICE_EN=Aiden

# 豆包 TTS V3（seed-tts-2.0）
DOUBAO_API_KEY=...
DOUBAO_RESOURCE_ID=seed-tts-2.0
DOUBAO_VOICE_ZH=zh_female_vv_uranus_bigtts
DOUBAO_VOICE_EN=en_female_stokie_uranus_bigtts
# DOUBAO_MODEL=seed-tts-2.0-standard   # 仅复刻音色时需要

# 可选：保护写操作（导入/生成/取消）；不设则局域网开放
# DICTATION_API_TOKEN=...
```

密钥永不写入仓库；缺失时提示可用 `--provider mock`。
密钥由 `serve` 独占读取（`/etc/dictation/env`，0600），agent 与 CLI 都不需要；
也可用 systemd `LoadCredential` 注入，代码优先读 `$CREDENTIALS_DIRECTORY`。

---

## 15. 功能块（大块）

> 只列大块；每块的完成状态、验收项、工作日志见 `TODO.md`。

- **F1 词表 → TTS 音频**：解析、逐词合成、精确间隔、缓存、中英、语速、开场白/结束语。
- **F2 儿童端播放器**：首页、播放/暂停/重听、完成、再听一遍、断点续听。
- **F3 自检 / 记录 / 结果查询**：显示所有词、只标错、SQLite attempts；
  `result list/show/wrong` 导出 JSON 供大模型分析。
- **F4 错词复习 / 自动混入**：错词入口、到期调度、下一课混入、「复习」标签。
- **F5 家长模式 / 答案纸**：调 gap/rate/repeat、A4 打印、错词本。
- **F6 拍照批改**：多模态预判、置信度、人工确认、错字统计。
- **F7 SBC 部署 / 本地 TTS**：zigbuild、systemd、断网降级。

---

## 16. 开发约定

- **不执行 `git commit` / `git push` / `git reset`**，除非用户明确要求。
- 不执行破坏性命令（`rm -rf`、清库）；清理缓存用 `dictation clean`。
- 每次改动后跑：
  `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、`cargo test`；
  前端改动加 `node --check web/app.js`。
- 单元测试不访问网络；provider 测试用 `mock` 或本地 mock server。
- **新增依赖优先纯 Rust；尽可能避免 `*-sys`**；若必须引入，先确认
  `cargo zigbuild --target riscv64gc-unknown-linux-gnu` 可编译，并在 TODO 记录理由。
- 所有用户可见文案用中文；代码标识符用英文。
- 时间/路径字段可序列化、跨平台。
- 不把 API key、孩子照片、真实姓名提交到仓库。
- **agent 写入必须走 HTTP API**（`POST /api/lessons` 等），不 exec CLI、不读写
  `/srv`、`/opt`；路径只由服务端从 `config.toml` 解析，不硬编码。
  CLI 是运维/本地调试入口，可以要求本机持有密钥。
- **HTTP 契约先改 `docs/http-api.md` 再写代码**；导入校验必须 CLI 与 HTTP 共用
  同一份实现（`lesson::import`），不允许复制。
- **同一能力同时有 CLI 与 HTTP 入口时，必须共用实现，语义一致**：
  - 读取/序列化放库里（`lesson::import`、`lesson::list`、`result_view`、`render`），
    CLI 与 `serve` 只做参数解析与输出；
  - 运维类操作（`lesson import/list/plan/gen`、`result list/show/wrong`、
    `config show`、`clean`，以及以后新增的 publish 等）**新增或修改时必须两侧同步**，
    只改一侧视为未完成；
  - 字段名、字段值、单位、排序、过滤、幂等行为一致；错误按载体映射但含义相同
    （HTTP 状态码 + `{"error"}` / CLI exit 1 + stderr `{"error"}`）；
  - 允许的差异只有运行时能力（job 队列/并发/取消、鉴权、媒体、session 与 check；
    CLI 侧的 `clean`/`plan`/`new`/`config show` 与路径模式 `gen`），且必须在契约写明。
  - 检查清单见 `docs/http-api.md` 的「CLI 与 HTTP 的一致性约定」。
- **所有新 CLI 必须 agent-first**：支持 `--json`、非交互、可脚本化；
  破坏性操作默认 dry-run，必须显式 `--apply`/`--force`；错误用
  `{"error": ...}` + 非零退出；路径/配置走 `AppConfig`。
- **完成工作后更新 `TODO.md`**（状态/勾选/日志）；只有设计变化才改本文件。

---

## 17. 风险与已知问题

| 风险 | 影响 | 对策 |
|---|---|---|
| TTS 多音字读错 | 听写内容错误 | `speak=` 覆盖；后续手工 wav 替换 |
| 英文孤立词句尾上扬 | 听感像问句/列举 | 默认 Stokie（美式女声）；Tim 备选；Dacey 不用 |
| 首尾静音不一致 | 间隔听感不准 | VAD 裁剪 + 固定静音 |
| 豆包 V3 音频块独立 padding | 拼接解码会静默错位、爆音 | 逐块 base64 解码再拼 PCM |
| Qwen `qwen3-tts-flash` 不支持 rate | 语速控制 | 后处理时长伸缩 |
| SBC 本地 TTS 未验收 | 离线不可用 | 云端为主；实测后再启用 |
| iOS 局域网 HTTP 无 SW | 无离线缓存 | SBC 常开；或 mkcert HTTPS |
| 手写识别误差 | 误判 | 固定答案纸 + 题号 + 置信度 + 人工确认 |
| 复习量过大 | 孩子抵触 | 默认 5，后续可调 |
| `-sys` 依赖 | riscv64 交叉编译失败 | 优先纯 Rust；zigbuild 验证；必要时替换存储 |

---

## 18. 待定 / 后续

- 英文听写比例与 voice 选择。
- 是否朗读 `note` 的组词/例句。
- 是否按字数自动计算 gap。
- 多音字词典（pinyin crate）自动生成 `speak`。
- 按教材/单元限定复习范围。
- 家长端 Web 编辑器（替代 agent skill）。
- 答案纸版式与拍照批改的对齐方式。
- SBC 本地 TTS 选型与验收。

---

## 19. Session 接手清单

1. 读本文件（稳定设计）。
2. 读 `TODO.md` 的「当前状态 / 下一步」，只做当前功能块。
3. `cargo test`；`cargo run -- plan fixtures/lesson-20.txt`。
4. 不要提前实现后续功能块，除非用户明确要求。
5. 完成后：
   - 更新 `TODO.md` 的状态、勾选、工作日志；
   - 只有设计/约定变化时才改 `AGENTS.md`；
   - 跑完整验证；
   - 回复里给出改动文件清单与验证结果。
