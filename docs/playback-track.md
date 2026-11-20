# 整轨播放（C1）实施说明

> **状态：已实施（2026-11-20）。** 本文件是实现记录 + 给
> 「分享发布（F8 / CF 导出）」的同步清单；稳定结论已进 `AGENTS.md` D16、
> 播放器细节进 `docs/player.md`、跨域结论进 `docs/share-publish.md` §11.2。
>
> 术语见本文件 §2；与 `docs/glossary.md` 里已有的「完整音轨」是**两个概念**，
> 不要混用。

---

## 1. 问题

孩子端听写时**熄屏 / 切 App / 来电**，播放中断。

### 1.1 根因（三层，缺一不可）

| # | 原因 | 证据 |
|---|---|---|
| 1 | **wake lock 从来没生效** | `Screen Wake Lock` 是 `[SecureContext]`，除 localhost 外的 `http://` 源上 `navigator.wakeLock === undefined`。项目按局域网 HTTP 部署，`requestWakeLock()` 每次都在 `if (!('wakeLock' in navigator))` 处直接 return。 |
| 2 | **时间轴建在 JS 计时器上** | 播放模型是「播 1.2s 片段 → `sleep()` 数 6–8s → 播下一段」。iOS 在屏幕锁定 / 后台时挂起 Web 进程 JS；Android Chrome 在页面**静音超过 30s** 后进入 intensive throttling，计时器降到 **1 次/分钟**，且官方明确「静音音频流不给豁免」。gap 本身就是静音，所以**第一个 gap 就断**。 |
| 3 | **应用自己补了一刀** | `visibilitychange → hidden` 时主动 `togglePause()`。熄屏、切 App、下拉通知栏都会变成一次显式暂停。 |

补充的 WebKit 已知问题（说明「换 src 续播」这条路走不通）：
`ended` 在后台/熄屏时不派发（WebKit #173332）；iOS 17.2+ 后台换
`src` + `play()` 事件正常但无声（WebKit #261554）。

### 1.2 已排除的方案

| 方案 | 结论 |
|---|---|
| HTTPS + Wake Lock | 只解决「自动锁屏」，切 App / 低电量 / 老 iOS / 独立 PWA 全失效；iOS 独立 PWA 的 wake lock 在 16.4–18.3 是坏的（WebKit #254545，18.4 才修）。**作为将来的可选增强，不作为主方案。** |
| NoSleep.js 隐藏视频 | 同 wake lock 的适用范围，且更耗电；0.12.0（2020-12）后未更新。 |
| 静音垫片常驻 | Chrome 明确「静音音频不给豁免」；iOS 照样挂起 JS。**无效。** |
| `ended` 事件换 `src` | 见 1.1 的两个 WebKit bug。**无效。** |
| Web Audio 精确排程 | 后台 `AudioContext` 被挂起；且 `audioSession=playback` 与录音采集冲突。**无效。** |
| 原生壳（Capacitor / Tauri） | 目标设备是锁定的学习机，装不了东西。**排除。** |

---

## 2. 术语（需登记进 `docs/glossary.md`）

| 词 | 代码标识 | 含义 | 不要用 |
|---|---|---|---|
| **片段** | `clip` / `clips[]` | 单个词/句的 wav，`clips/<hash>.wav` | 音频块、音源 |
| **完整音轨** | `full_track` | 服务端渲染落盘的 `full.wav` / `full.mp3` | 全长音频、混音 |
| **整轨** | `track` | **播放时在客户端按 manifest 拼出的单条 wav（内存 Blob），只存在于浏览器** | 完整音频、合并音频、完整音轨 |
| **拼装** | `assemble` | 把片段 + 静音按时间轴拼成整轨 | 合成、合并（那是 TTS 侧） |
| **分片引擎** | `clip engine` | 现有「逐片段播放 + 计时器」实现，保留为回退 | 旧播放器 |
| **整轨引擎** | `track engine` | 本文件描述的播放路径 | 新播放器 |

「整轨」与「完整音轨」是不同东西：前者是内存里的 Blob，后者是磁盘上的文件。
文档和 UI 文案都不要互相替换。

---

## 3. 方案

**播放一条持续的单资源媒体**，让 OS 把这一课当音乐/播客对待：熄屏继续放、
后台不节流、锁屏可控制。节奏（静音长度）存在于**音频样本**里，不再依赖 JS 定时器。

两种实现形态，本文件选 C1：

| | C1 客户端拼装（采用） | C2 服务端整轨 + Range（暂不做） |
|---|---|---|
| 后端改动 | 无 | `media()` 白名单放行 `full.*`、改流式 Range、`--with-full` 默认开 |
| 长按重生成 | 只重取 1 个片段（几 KB），本地打补丁 | 要重下整条 8–10 MB |
| 动态 gap（隐藏节奏面板） | 天然支持（拼装时用） | 不支持（除非服务端重渲染） |
| 远程 / 分享流量 | ≈ 1.1 MB（静音本地生成，不走网络） | ≈ 6.8 MB（整轨文件） |
| 内存 | 7–15 MB 常驻 | 小（流式） |

C1 的字节成本很低：20 词 × 6s gap ≈ 1.1 MB 语音 + 5.8 MB 静音 ≈ 7 MB，
memcpy 只要 5–40 ms。**真正的成本是「N 次 fetch」和「常驻内存」**，
不是拼接本身。因此**不做 IndexedDB 缓存整轨**（写 8 MB 比重拼还慢）。

---

## 4. 数据与算法

### 4.1 输入（全部来自 manifest，无新字段）

| 字段 | 用途 |
|---|---|
| `defaults.lead_in_ms` | 开头静音（仅从头播放时） |
| `defaults.intro_gap_ms` | 开场白之后、第一个词之前 |
| `defaults.gap_ms` / `defaults.repeat` / `defaults.repeat_gap_ms` | 词后等待 / 遍数 / 遍间 |
| `intro` / `outro`（`{path, duration_ms}`） | 开场白 / 结束语 |
| `items[i].clips[0]`（`{path, duration_ms}`） | 片段 |
| `items[i].repeat` / `repeat_gap_ms` / `gap_ms` | 词条级覆盖 |
| `items[i].base`（会话注入）或 `manifest.base` | 媒体根路径 |
| `total_ms` | 自检用：拼完的文件时长应与之相符（容差 ±200ms） |

时间轴（与 `docs/player.md` 一致）：

```
lead_in
→ [intro] → intro_gap
→ item 0: clip × repeat（遍间 repeat_gap）→ gap
→ item 1: ...
→ [outro]
```

### 4.2 解析 WAV：**不要硬编码 44 字节头**

`clips/*.wav` 由 hound 写出（24 kHz / mono / s16le），当前结构稳定，但正确做法是
遍历 RIFF chunk 找到 `data`：

```js
function pcmData(buf) {
  const dv = new DataView(buf);
  if (dv.getUint32(0, false) !== 0x52494646) throw new Error('不是 RIFF'); // 'RIFF'
  let off = 12;
  while (off + 8 <= buf.byteLength) {
    const id = dv.getUint32(off, false);
    const size = dv.getUint32(off + 4, true);
    if (id === 0x64617461) return new Uint8Array(buf, off + 8, size); // 'data'
    off += 8 + size + (size & 1);
  }
  throw new Error('没有 data 块');
}
```

同时校验 `fmt ` 块：采样率 24000、声道 1、位深 16。不一致就不要拼，
走回退（见 §7）。**不要重采样**——那是服务端的职责。

### 4.3 静音：复用零缓冲

`new Uint8Array(n)` 本身就是全零，不需要再填。多个长度相同的 gap 复用同一个
`Uint8Array` 实例即可（Blob 构造按引用收集，不复制）。

### 4.4 结构

```js
// 一次会话内的状态
track = {
  parts: [],        // [header, chunk0, chunk1, ...]，给 new Blob(parts) 用
  chunks: [],       // 每个 chunk 自己的 parts 列表（重生成时只重建一项）
  starts: [],       // 每个 item 在整轨里的起始秒（含 lead_in / intro）
  bytes: 0,         // 总字节数
  url: null,        // createObjectURL 的结果，替换前必须 revoke
  durationMs: 0,    // 整轨时长（= total_ms 量级）
}
```

### 4.5 拼装伪代码

```js
async function assembleTrack(manifest, runId) {
  const parts = [wavHeader(totalBytes)];   // 先算总量再写头
  const chunks = [], starts = [];
  let cursorMs = 0;

  cursorMs += leadInMs;  parts.push(silence(leadInMs));       // 从头播放才有
  if (intro) { parts.push(clipPcm(intro)); cursorMs += intro.duration_ms; }
  cursorMs += introGapMs; parts.push(silence(introGapMs));

  for (let i = 0; i < items.length; i++) {
    const it = items[i];
    starts[i] = cursorMs / 1000;
    const gap = customGapMs === null ? it.gap_ms : customGapMs;   // 见 4.6
    const chunk = [];
    for (let r = 0; r < it.repeat; r++) {
      const pcm = await fetchClip(it, i, runId);   // 带 ?v=，失败抛错
      chunk.push(pcm);
      cursorMs += pcm.durationMs;
      if (r < it.repeat - 1) { chunk.push(silence(it.repeat_gap_ms)); cursorMs += it.repeat_gap_ms; }
    }
    chunk.push(silence(gap)); cursorMs += gap;
    chunks[i] = chunk;
    parts.push(...chunk);
  }

  if (outro) { parts.push(clipPcm(outro)); cursorMs += outro.duration_ms; }

  return { parts, chunks, starts, bytes, durationMs: cursorMs, url: URL.createObjectURL(new Blob(parts, { type: 'audio/wav' })) };
}
```

**总量要先算后写头**：WAV 头里的 `RIFF` 大小和 `data` 大小是固定字段，
所以要么两趟（先累加算总字节，再拼），要么预留头位置最后回填。
推荐两趟，逻辑最简单。

### 4.6 动态 gap 的 80 ms 修正（容易漏）

隐藏节奏面板给的是**可听静音**。现有分片引擎在 `timingGapMs()` 里已经做了：

```
插入静音 = 用户设定的毫秒数 - CLIP_TAIL_MS(80)
```

因为片段自带 20 ms 前导 + 60 ms 尾音。**整轨拼装必须复用同一个换算**，
否则家长设「6 秒」会变成 6.08 秒，而且只在开了隐藏面板的课上出现。

### 4.7 进度映射

- 显示进度：`starts[i] <= currentTime < starts[i+1]` → 第 `i` 项。二分查找。
- 不要用 `timeupdate` 计数推算（后台会被挂起）；**永远以 `currentTime` 为准**。
- `timeupdate` 约 4 次/秒，足够刷新进度条；不要给它加别的逻辑。

---

## 5. 播放控制

沿用现有 DOM 与事件绑定，只换引擎实现。

| 行为 | 现在的实现 | 整轨实现 |
|---|---|---|
| 开始 | `startPlayback()` + `runTimeline()` | 先 `assembleTrack()`（显示「准备音频…」），再 `trackEl.play()` |
| 暂停 / 继续 | `audio.pause()` / `play()` + 冻结 `sleep` 计时 | `trackEl.pause()` / `play()`，无计时器参与 |
| 重听当前词 | 单独播一次片段，再回到 gap | `seek(starts[player.index])` 并继续播放 |
| 上一题 / 下一题 | `jumpTo()` 重起 runId | `seek(starts[target])`，暂停态保持暂停 |
| 完成 | `finishPlayback()` | 同（见 §5.2 的 `ended` 兜底） |
| 返回 | `stopPlayback()` | 暂停 + `revokeObjectURL` |
| 续听 | 跳过 lead_in / intro，`RESUME_LEAD_MS=300` 后从第 i 项开始 | 照常拼整条（**不要从 i 开始拼**），然后 `seek(starts[i])` |

**「重听」语义有细微变化**：现在只重播一遍片段就回到 gap；整轨下是「跳回该词起点
继续走完」。对孩子的体感一致（又听到一遍），但会多消耗一次 repeat 的播放。
这是可接受的行为变化，需在 `docs/player.md` 写明。

### 5.1 元素

新增一个专用元素 `<audio id="track" preload="auto" playsinline>`，不要复用 `#audio`：

- `#audio` 已经承担「解锁」和检查页 `playPreview()` 的单听；
- 两者共用会让 `ended` 事件互相串。

`unlockAudio()` 需要在同一次用户手势里**同时**解锁两个元素（各播一小段静音 wav）。

### 5.2 `ended` 不可靠，用 `currentTime` 兜底

熄屏时 `ended` 可能延迟到页面恢复才派发。规则：

```js
// 页面恢复可见 / 定时（前台时）检查
if (trackEl.ended || trackEl.currentTime >= track.durationMs / 1000 - 0.3) finishPlayback();
```

不要在 `ended` 里做唯一判定。

### 5.3 播放前预热

iOS 首次 `play()` 有 100–300 ms 解码延迟。拼装完成后先 `trackEl.load()`，
等 `loadedmetadata`（或 `canplay`）再 `play()`，否则「点了开始没声音」会被误判成失败。

---

## 6. 长按重新生成（现有后门）如何衔接

### 6.1 现状（必须理解，否则会写错）

- 客户端：`regenerateDuringPlayback(item, lessonId)` → `regenerateWordAndWait()` **轮询 job 到 done**（约 1 秒）→ `state.clipVersions.set(text, Date.now())`。
- 服务端：`regenerate_word()` = **清该词的 TTS 缓存 → `enqueue_generation()` 整课重渲染 → 返回 job**。
  只有那一个词真的调 TTS，其余命中缓存；但 **`manifest.json` 和 `full.wav` 都会被重写**。
- 片段文件名对同一个词是**稳定的**，重新生成会**原地覆盖同名文件**，所以 `media()` 只敢发 `Cache-Control: no-cache`，客户端靠 `?v=` 破缓存。

### 6.2 补丁流程（整轨引擎）

job 完成后，**不要重建整条整轨**，只换那一个 chunk：

1. 重新 `GET /api/sessions/{id}` 或重读当前 manifest（**因为 `duration_ms` / `total_ms` 变了**）；
2. 用新的 `?v=` 重新 `fetch` 该词片段 → §4.2 取 PCM；
3. 重建 `chunks[i]`（`repeat` 份 + `repeat_gap` 静音 + `gap` 静音）；
4. 用新的 `chunks[i]` 替换 `parts` 里对应的那一段；
5. **从 `i` 起重算 `starts[]`**（新采样时长几乎一定不同，后面所有偏移都要平移）；
6. `URL.revokeObjectURL(track.url)` → `track.url = createObjectURL(new Blob(parts))`；
7. `trackEl.src = track.url; trackEl.load();` 等到 `canplay` 后 `seek(starts[i])` 并播放。

第 5 步是必须的：偏移不平移，后面所有词的「重听 / 上一题」都会错位。

### 6.3 边界

- **重生成期间冻结进度更新**：`state.regenerating` 为真时不根据 `currentTime` 改 `player.index`，否则会拿旧 `starts[]` 算。
- `repeat > 1` 时同一段 PCM 在 chunk 里出现多次，一并替换（重建整个 chunk 天然覆盖）。
- **static（分享）模式没有这个功能**（`CAP.regenerate === false`，`staticApi.regenerateWord` 直接 reject），所以只有 `?v=` 会在服务端模式出现。分享版拿到的 URL 没有 `?v=`，对 `immutable` 缓存友好。

---

## 7. 双引擎与自动降级

**不要删掉分片引擎。** 保留 `sleep / playUrl / runTimeline / replayItem / sleepWithReplay`，
用开关选择，失败自动降级：

```js
// 优先级：URL 参数 > localStorage > 默认
// ?track=0 / ?track=1   —— 钉钉里没有 devtools，必须能用 URL 关掉
state.trackMode = readFlag('track', true);
```

降级触发条件（任一）：

| 条件 | 处理 |
|---|---|
| 任一片段 `fetch` 失败 / 404 | 降级，提示「改用逐词播放」 |
| WAV 头不合规（非 24k / 非 mono / 非 s16） | 降级（说明数据管道变了） |
| `new Blob()` 抛错、超预算（建议阈值 32 MB） | 降级 |
| 首次 `play()` 报错 | 降级 |
| 环境不支持 `Blob` / `createObjectURL` | 直接走分片引擎 |

**降级时**：`visibilitychange` 自动暂停那条要恢复（见 §8），否则分片引擎会漂移。

---

## 8. 要改 / 要删的现有代码

| 位置 | 改动 |
|---|---|
| `visibilitychange` 处理里的 `togglePause()` | 整轨模式下删掉（保留为分片引擎分支） |
| `requestWakeLock()` / `releaseWakeLock()` | **保留**，但降级为增强：整轨模式已不需要它；`navigator.wakeLock` 存在且页面可见时才请求 |
| `startPlayback()` | 改为「先拼装 → 再播放」，失败走 `runTimeline` |
| `stopPlayback()` / `finishPlayback()` | 增加 `revokeObjectURL` 与元素复位 |
| `jumpTo()` / `requestReplay()` / `togglePause()` | 改为操作 `trackEl` |
| `saveResume()` | **不用改**：`{sessionId, manifest, index}` 正好够重建 |
| `unlockAudio()` | 同时解锁 `#audio` 与 `#track` |
| `index.html` | 新增 `<audio id="track" ...>` |

`docs/player.md` 的「播放器时间轴」小节需要重写为整轨语义（保留分片引擎为回退）。

---

## 9. 给「分享发布（F8 / CF 导出）」会话的同步清单

> 这一节是**跨会话契约**，最容易漏，且只在远程复现。

### 9.1 必须改：R2 的 CORS 规则要覆盖 `clips/*`

`docs/share-publish.md` §11.2 现在写的是：

> CORS（仅为 JSON 的 fetch 服务）—— `<audio src>` 跨域不需要 CORS；`fetch(lesson.json)` 需要。

**这条结论被整轨引擎推翻了**：每个片段都变成 `fetch()`，所以 R2 必须对
`clips/*.wav` 也返回跨域头。否则症状是：

> 本地一切正常 → 发布到 CF → 首页能开、课时能进 → **点「开始」直接失败**。

§11.2 需要改成「JSON 与 **片段** 都需要 CORS」，示例配置覆盖整个桶：

```json
[
  {
    "AllowedOrigins": ["https://dictation.example.com"],
    "AllowedMethods": ["GET", "HEAD"],
    "MaxAgeSeconds": 3600
  }
]
```

（不需要额外 `AllowedHeaders`：`fetch` 不带自定义头；`?v=` 是 query，不影响 CORS。）

### 9.2 不用改，但要确认

- **`clips/*.wav` 的 `Cache-Control: immutable` 保持**。分享版片段名含内容哈希、不会原地覆盖；`?v=` 只在服务端模式出现（§6.3），两边不冲突。
- **不要删除 `lesson.json` 里的 `items[].clips[].path` 与 `duration_ms`**。整轨拼装直接依赖这两个字段；`duration_ms` 缺失会让静音/偏移算错。如果正在改 `publish` / `--reindex` / `--shell` 的结构，**保留这两个字段的语义**。
- **片段不要转成 mp3**。整轨按字节拼 WAV；要省流量属于协议变更，需单独评估（见 §12）。

### 9.3 `--with-full` 与 `full.mp3` 的定位变化

- 整轨引擎**不再需要** `full.mp3`：播放器自己拼。
- 但 `full.mp3` 仍有独立价值：**给普通播放器兜底 / 下载后用系统播放器听**（正好是「离开主应用」的应急路径）。
- 建议：**保持现状（默认不带，`--with-full` 可选）**，不要因为整轨上线就删掉这个能力。

### 9.4 进度与同源

- 分享链接是 `?l=<lesson_id>`，与主应用**同源**，所以 `localStorage` 里的续听进度共享；整轨引擎不改变这一点。
- 若将来分享链接换域名，续听进度不会跟着走（`localStorage` 按源隔离）。这条与整轨无关，但一并记录。

### 9.5 合并顺序建议

当前工作区有较多未提交改动，两边都会碰 `web/app.js`。建议：

1. 先把**分享发布侧**的改动成块提交（`CAP`、`staticApi`、深链、导入结果、首页卡片）；
2. 再叠**整轨引擎**的提交（`#track` 元素 + 新增 `track engine` 段落 + 相关函数替换）；
3. 播放引擎与发布逻辑**不要在同一次提交里混改**，否则回滚困难。

---

## 10. 验收

### 10.1 设备矩阵

| 设备 / 入口 | 关注点 |
|---|---|
| iPhone + Safari | 基准；同时验证「添加到主屏幕」后的行为 |
| Android + Chrome | 基准；后台节流是否真的消失 |
| **学习机 + 钉钉内置 WebView** | 最大不确定性：宿主 App 可能自己停音频 |
| 微信内置（Android） | X5 内核，大 blob 风险 |

### 10.2 三条判据

| # | 操作 | 通过标准 |
|---|---|---|
| T1 | 开始播放 → 5 秒内熄屏 → 等 2 分钟 → 恢复 | 音频**连续**，词间隔听感与不带熄屏时一致；恢复后进度正确 |
| T2 | 同上，恢复后读 `currentTime` | 与真实播放进度一致（误差 < 1s），证明没用计时器推算 |
| T3 | 40 词 × 8s（约 15 MB） | 不 OOM、不中断；拼装时间 < 2s |

补充：低电量模式、来电中断、切 App 再回来。

### 10.3 钉钉里怎么取证

没有 devtools，必须让页面自己说话。建议加一个 `?diag=1` 覆盖层，记录：

- `navigator.wakeLock` / `navigator.audioSession` / `navigator.mediaSession` 是否存在
- UA、`document.visibilityState` 变化时间线
- `track.bytes` / `track.durationMs` / `audio.duration`
- `currentTime` 的采样点（每次 `visibilitychange` 与恢复时各一条）
- 降级原因（若有）

用现有的 `showExportText()` 把 JSON 贴出来即可，不需要新 UI 体系。

### 10.4 每次改动后的常规检查

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
node --check web/app.js
```

前端改动不涉及 Rust，但 `media()` / manifest 若动过就要跑全套。

---

## 11. 不做的事

| 不做 | 原因 |
|---|---|
| Service Worker / 离线 | 在家是局域网、在外是 CF，都有网；且 SW 缓存与「原地覆盖片段 + `?v=`」天然冲突，会制造「重新生成了还听到旧版」 |
| IndexedDB 缓存整轨 | 写 8 MB 比重拼（5–40 ms）还慢；整轨是 manifest 的纯函数，随时可重建 |
| MediaSource / MSE | iOS 只对视频支持且受限，收益为零 |
| HTTPS（本次） | 与音频修复正交；将来用「CF 灰云 A 记录 + Let's Encrypt DNS-01」即可，钉钉等 WebView 直接认证书，不用装 CA |
| 给学习机装浏览器 | 需要 ADB / 直装包 / 刷机绕过厂商白名单，随固件升级失效，不能作为工程前提 |

**PWA 备注**：要「放桌面菜单」不需要 HTTPS，也不需要 PWA ——
`apple-mobile-web-app-capable`（`web/index.html` 已有）+ `apple-touch-icon`
（**待补**，当前 manifest 用的是 `data:` SVG，iOS 桌面图标基本不生效，会显示截图/首字母）
就够。Android Chrome 在 HTTP 下只能创建快捷方式，真安装需要 HTTPS + SW。

---

## 12. 风险与回退

| 风险 | 影响 | 对策 |
|---|---|---|
| 宿主 WebView 自己停音频（钉钉/微信） | 整轨也救不了 | 先按 T1 实测；不行则换设备（iPhone Safari / 安卓 Chrome 平板） |
| 大课时内存（>32 MB） | OOM / 卡顿 | 超阈值降级到分片引擎，或提示「本课较长，请保持屏幕常亮」 |
| blob 音频在旧内核上异常 | 无法播放 | 自动降级；必要时走 C2（服务端整轨 + Range） |
| 片段非 24k/mono/s16 | 拼出来是噪音 | 校验 `fmt ` 块，不合规就降级 |
| 重生成后偏移没重算 | 后续词错位 | §6.2 第 5 步写成断言：`starts` 长度必须等于 items 长度 |
| 整轨与 `full_track` 混淆 | 文档/代码用词混乱 | §2 术语先登记，再写代码 |

---

## 13. 落地记录（2026-11-20 完成）

1. ✅ `#track` 元素 + `unlockAudio` 双解锁 + `?track=0/1` 开关；
2. ✅ `assembleTrack()` + `readWavPcm()`（遍历 RIFF + 校验 24k/mono/s16）+
   静音复用 + 80 ms 换算（复用 `timingGapMs()`）；
3. ✅ `startPlayback` / `togglePause` / `requestReplay` / `jumpTo` 切到整轨；
   进度以 `currentTime` + `starts[]` 二分为准，`ended` 不派发时按「接近结尾」兜底；
4. ✅ 降级：片段 404 / 格式不合规 / >32MB / `play()` 失败 / 无 Blob 能力 →
   清整轨并提示「改用逐词播放」，本会话不再尝试；
5. ✅ 重掷补丁：只重建该 item 的 chunk，重算后续 `starts[]`，换 Blob 后 seek 回原位；
6. ✅ `?diag=1` 覆盖层（visibility 时间线、track/audio 时长、currentTime、降级状态）；
7. ✅ 文档：`AGENTS.md` D16、`docs/player.md`、`docs/share-publish.md` §11.2、
   `docs/glossary.md`、`TODO.md`。

验证（桌面 Chrome + serve 真数据）：

- 20 词课：`trackMs=200409` 与 `manifest.total_ms=200410` 相差 1 ms；9.6 MB Blob；
- 暂停/继续、上一个/下一个、重听（seek 回词首）均正常；
- 静态模式（fixture）：216 KB 整轨、4.5 s 完整播完，检查页照常；
- `?track=0` 回退分片引擎，进度照常推进。

**尚未验证**：真机熄屏 T1/T2（需要 iPhone/Android 实机）、钉钉/微信 WebView、
长按重掷在整轨下的补丁（需要真实 TTS）。