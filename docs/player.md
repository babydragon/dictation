# 儿童端播放器与自检设计

> 本文件定义 PWA 的页面、播放时间轴、检查页与交互约定。
> 术语与命名以 [`glossary.md`](glossary.md) 为准；HTTP 接口、SQLite schema、
> 鉴权见 [`http-api.md`](http-api.md)；整体设计见 [`AGENTS.md`](../AGENTS.md)。
> 最后更新：2026-11-18

## 范围

- 首页：课时列表 + `错词复习 (N)` 入口 + 「继续上次听写」。
- 播放：按 manifest 播放 intro → 词条 → outro；暂停/继续、重听当前词、再听一遍。
- 完成页：`检查`、`再听一遍`、`返回首页`。
- 检查页：显示本次所有词；只标错，未标默认正确；每行可单听；提交后落库。
- 记录：SQLite 存 attempts 与 review_state。
- 错词复习：首页入口，按到期错词生成一个复习 session。
- 课内复习：课时卡片上的次级入口 `复习 N`，只取**本课**待复习词；
  孩子端唯一的复习入口，不提供范围/数量设置（跨课复习由 agent / 家长端发起）。
- 自动混入下一课：本课之后追加按比例（25%，上限 8）的待复习词
  （`review=true`，音频仍来自原课）。
- 断点续听：`localStorage` 保存 session + 当前 item，7 天内可继续。
- 不做：拍照批改、答案纸打印、账号、多用户、离线 Service Worker。

## 文件归属

| 范围 | 文件 |
|---|---|
| 前端 | `web/index.html`、`web/style.css`、`web/app.js`、`web/manifest.webmanifest` |
| 交互契约 | `docs/player.md` |

前端不修改服务端文件；静态资源由 axum 直接托管，无构建步骤。

## SessionManifest

服务端（`POST /api/sessions`、`POST /api/sessions/review`）返回给播放器的 JSON。
课时示例：

```json
{
  "session_id": "5b0d...",
  "kind": "lesson",
  "title": "第5课",
  "subject": "chinese",
  "grade": 3,
  "term": 1,
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
- `subject` 为 `chinese`/`english`；`grade`/`term` 可能为 `null`。复习 session 沿用请求的科目。
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

## 断点续听

- `localStorage` 存 `{sessionId, manifest, index, savedAt}`；每个 item 开始、暂停、
  `pagehide`/`visibilitychange` 时更新。
- 首页显示「继续上次听写」（标题 + `3 / 20` + 时间），7 天过期。
- 从保存的 item 继续，跳过 lead-in/intro；完成、提交检查、清除、音频失败时删除。
- 「不继续」需二次确认。

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
- 按科目分别显示 `错词复习 (N)` 卡片（`语文` / `英语`），点击 POST
  `/api/sessions/review`（body 带 `subject`）并进入播放器；没有积压的科目不显示。
- 课时卡片：标题、科目、词数、预计时长；点击 POST `/api/sessions` 并进入播放器。
- **课内复习**（2026-11-18，待实现）：课时卡片上，当该课 `review_count > 0` 时显示
  次级动作 `复习 N`，点击 POST `/api/sessions/review`（body 带 `subject` + `lesson_id`）。
  - `N` 来自 `GET /api/review/lessons`（一次性拿所有科目，不需要按科目拆请求）；
    `review_count = 0` 时不显示。
  - 只有这一个复习入口，**不给孩子**范围选择、数量选择或「整课重排」。
  - 卡片当前是整块 `<button>`，加次级动作需改成 `div.card` + 主区域点击 +
    独立按钮（不能嵌套 `<button>`）。
  - 返回 409（无待复习词）：toast 提示「这一课没有需要复习的词」并留在首页。
- 空状态：`还没有课时，先用 dictation gen 生成一课`。
- 加载失败：显示重试按钮。

## 隐藏的调试入口（家长用，不提示孩子）

- **听写节奏面板**：长按首页标题「听写」1.5 秒打开；正常点击/短按无反应，滑动取消。
  - 「每个词后等待」输入秒数（0–120，步进 0.5，一位小数）。
  - 换算：`gap_ms = round(秒数 × 1000) − 80`（片段自带 20ms 前导 + 60ms 尾音），
    下限 0，所以**可听静音 ≈ 输入的秒数**。
  - 只覆盖词后等待；`repeat_gap_ms`、`lead_in_ms`、`intro_gap_ms` 不变。
  - 存 `localStorage["dictation.timing.v1"] = {"gapSeconds": 10}`；「恢复默认」删除该键。
  - 纯客户端：所有会话生效，离线/静态发布同样可用（每个设备一份）。
- **单词重新生成**：两处入口，都是长按 1.5 秒 + 确认框：
  - 播放页长按「重听这个词」→ 重掷**当前这个词**（45 词的长课不用等检查页）；
  - 检查页长按某个**词文本**。
  都调 `POST /api/lessons/{lesson_id}/regenerate-word {text}`，前端轮询
  `GET /api/jobs/{id}` 到 `done` 才提示成功（最长 45 秒）。
  重掷后片段是同名覆盖，重听时 URL 追加 `?v=<时间戳>` 强制回源。
  - 长按必须吞掉松手后补发的 click，否则刚弹出的确认框会被立刻点掉。
  - **Android 兼容**：手机自带浏览器/微信长按会先启动「选中文字」手势，
    只给元素加 `user-select: none` 不够；必须在 `html, body` 再禁一次，
    并用 `selectstart` 捕获阶段 `preventDefault`（输入框除外）。
  - 微信内置 WebView 会把长按转成 `contextmenu`：拦下系统菜单并当作长按触发，
    与 pointer 计时 800ms 内去重。
  - `lesson_id` 从 `player.manifest.base` 的 `/media/<id>` 解析；
    `kind != "lesson"`（复习/自定义会话）不启用。
  - 成功后 toast「已重新生成，下次进入这一课生效」；正在播放的会话仍用旧片段。
  - 目的：TTS 采样是非确定性的，内容哈希缓存会把一次坏采样固定下来，长按等于「重掷」。

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
- 全屏、Wake Lock、屏幕常亮。

## 测试

- 用真实服务端手动走一遍：首页 → 播放 → 完成 → 检查 → 提交 → 首页复习入口。
- 课内复习：本课标错 → 首页该课出现 `复习 N` → 进入只含待复习词 → 标对 → 入口消失。
- 自动混入：A 课标错 → B 课出现「复习」词 → 标对后入口消失。
- 断点续听：播放到 2/3 → 刷新 → 首页出现继续卡片 → 从 2/3 继续 → 完成后卡片消失。
- 视口 320 / 375 / 390px：检查页标签与底栏不遮挡。
