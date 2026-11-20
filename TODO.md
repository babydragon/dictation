# TODO / 进度与验收

> 本文件是**进度、验收、工作日志的唯一来源**。
> `AGENTS.md` 只保留稳定设计与约定；每次工作完成后更新本文件，不要为了进度去改
> `AGENTS.md`（除非设计本身变了）。
>
> 术语与命名见 [`docs/glossary.md`](docs/glossary.md)，本文件用词以词表为准。
> 最后更新：2026-11-19

---

## 1. 当前状态

| 功能块 | 状态 |
|---|---|
| F1 词表 → TTS 音频 | ✅ 完成；豆包 V3 已验证；Qwen 代码保留但未验证（暂用火山） |
| F2 儿童端播放器 | ✅ v1 完成；桌面 + 手机/平板实机验证 |
| F3 自检 / 记录 | ✅ v1 完成 |
| F4 错词复习 / 自动混入 | ✅ v1 完成 |
| F5 家长模式 / 答案纸 | ⬜ 未开始 |
| F6 拍照批改 | ⬜ 未开始 |
| F7 SBC 部署 / 本地 TTS | 🚧 交叉编译已通过；待 SBC 运行 / systemd / 本地 TTS |
| SBC 交叉编译审计（-sys） | ✅ `cargo zigbuild` 编译通过；待板上运行验证 |
| Agent 导入（config.toml + CLI） | ✅ `lesson import/plan/gen/list` + `config show` |
| 结果查询 / 错词导出 | ✅ `result list/show/wrong`，JSON 供大模型分析 |
| 结果查询 HTTP 化 | ✅ `GET /api/results[/{id}|/latest][/wrong]`，与 CLI 同一序列化 |
| CLI/HTTP 一致性 | ✅ 导入、列表、结果共用实现；约定见 `docs/http-api.md` |
| 科目隔离 / schema v2 | ✅ 迁移 + subject/grade/term 贯通 |
| 复习组卷算法 | ✅ review_cap 生效、优先级、比例、洗牌、150s 护栏 |
| 错词复习 | ✅ `limit`(1–40)、`grade`/`term` 筛选、科目卡片显示逾期 |
| 范围复习 | ✅ `/api/review/queue`、`/api/sessions/custom`、`GET /api/sessions/{id}`、`?s=` 深链 |
| 复习第二轮（课内复习 + 范围复习） | ✅ schema v4 / 排序 / 提前复习调度 / `lesson_id` / `/api/review/lessons` / `/api/words` / `custom.shuffle` / PWA「复习 N」 |
| 播放页导航 | ✅ 上一个 / 下一个；暂停态跳转保持暂停，首尾置灰 |
| 删除课时 | ✅ `lesson delete` / `POST .../delete`（默认 dry-run，保留历史、词音源重指向）|
| 重复课时报告 | ✅ `lesson duplicates` / `GET /api/lessons/duplicates` |
| 单词重新生成 | ✅ `POST /api/lessons/{id}/regenerate-word` + 检查页长按（删缓存样本、只花 1 次 TTS） |
| 听写节奏（隐藏） | ✅ 长按首页标题设置词后等待（本地设置，静态发布同样可用） |
| 分享发布 F8（前端） | 🚧 static 模式 + `?l=` 深链 + 本地判分 + 结果导出已实现；导入闭环未开始 |
| 分享发布 F8（publish） | 🚧 `publish` CLI/HTTP + `--shell`（B 方案 Pages 工程）已实现；`--status/--reindex/unpublish` 未开始 |
| 分享结果导入（F8） | ✅ `result import` CLI/HTTP 幂等入账（schema v5）+ 家长入口；真实 R2 冒烟待做 |
| 熄屏播放（整轨引擎 C1） | ✅ 客户端拼装整轨 + 分片引擎回退 + 重掷补丁 + `?diag=1`；待真机 T1/T2 |
| 缓存清理 | ✅ `clean --unused/--older-than`，默认 dry-run |
| 断点续听 | ✅ localStorage 保存会话 + 条目，7 天有效；删除/重建课时的进度校验（server 续播走新会话，static 校验 content_hash）|

当前测试：`cargo test` **200 passed**（191 lib + 9 bin）；clippy / fmt 干净；
`node --check web/app.js` 通过。
演示课时：`lessons/第5课`（20 词，3:24，运行时数据，不提交）。

## 2. 下一步（按优先级）

> SBC 部署已验收（安装 + systemd 自启 + 手机访问 + 服务端生成），
> 不再是待办。离线本地 TTS 仍为可选增强。

1. **overrides + sample**：把挑中的采样固化到 `<cache>/overrides/<key>.wav`，配
   `dictation sample <词> --takes N` 出候选；解决「坏采样被缓存锁死」的最终方案。
2. **家长模式 / 答案纸**：A4 一列 20 题、带题号（gap 的运行时调节已由隐藏面板覆盖）。
3. **拍照批改**：`Grader` trait + 多模态模型 + 置信度 + 人工确认。
4. **记忆曲线细化**：FSRS、按教材/单元限定范围（当前用课时范围表达）。
5. **本地 TTS（可选）**：断网兜底，SBC 上选型与实测。
6. **F8 外网分享**：Cloudflare Pages + R2。前端（static + 导出）、发布
   （`publish`）、导入闭环（`results/import` + 家长入口）、熄屏整轨引擎均已完成；
   剩 `--shell/--status/--reindex/unpublish`、skill 更新、真实 R2 冒烟与真机
   T1/T2（熄屏）验证。契约见 `docs/share-publish.md`（需入库）。

### 发布 v0.2.0-rc.1（科目 + 复习组卷）

- [x] `Cargo.toml` / `Cargo.lock` 升到 `0.2.0-rc.1`
- [x] 全量验证：fmt / clippy / 188 tests / JS / bash -n / x86 + riscv64
- [x] commit + push + tag `v0.2.0-rc.1`（CI 自动 `--prerelease`）
- [x] 校验 release 资产、`SHA256SUMS`、RISC-V ELF
- [x] 从 release 安装一次（`--version v0.2.0-rc.1`）并跑迁移 + HTTP 生成

### 发布 v0.1.0（正式版）

本地 agent 已按 HTTP 闭环验证通过（导入 → 生成 → 播放/自检）。

- [x] `Cargo.toml` / `Cargo.lock` 升到 `0.1.0`
- [x] README/INSTALL/install.sh 示例改为 `v0.1.0`
- [x] 全量验证：fmt / clippy / 143 tests / JS / bash -n / x86 + riscv64
- [x] commit + push + tag `v0.1.0`；CI 未加 `--prerelease`
- [x] 校验 release 资产、`latest` 链接、`--data-root` 安装、HTTP 生成

### 发布 v0.1.0-rc.2（已完成）

- [x] rc.1：CI 全流程、pre-release、从 release 安装验证
- [x] 文档设备泛化 + 最低配置要求
- [x] 密钥/运行时文件扫描（`.env`、`data/`、`lessons/`、`cache/`、`demo/` 均 gitignore）
- [x] Git 仓库初始化：`https://github.com/babydragon/dictation`（public）
- [x] License：MIT（依赖均为宽松许可）
- [x] `install.sh --data-root`：支持 `/srv/dictation` 或 `/var/lib/dictation`
- [x] 配置/unit 自动使用 data-root；已有 config/env/data 保留
- [x] 本地 `--root + --data-root /var/lib/dictation` 幂等实测
- [x] 删除旧 `v0.1.0-rc.1` release + tag

### S1：agent 走 HTTP + 密钥收口（rc.2 内容）

设计目标：agent 零文件系统依赖（不 exec、不读路径），TTS key 只在服务进程。

- [x] 文档先行：`docs/http-api.md` 新增服务端生成端点契约
- [x] `AGENTS.md`：D13 改为 HTTP；新增 D13b 密钥归属；架构图/目录/env/约定同步
- [x] `skills/dictation-prep/SKILL.md` 改为 HTTP 流程；CLI 降为运维入口
- [x] `README.md` / `INSTALL.md` / `deploy/env.example` / unit 同步
- [x] 测试先行：契约里的服务端生成用例（导入、生成、job、token、cancel、body 解析）
- [x] 代码：抽出 `lesson::import`，CLI 与 HTTP 共用校验
- [x] 代码：`serve` 增加 `POST /api/lessons`、`generate`、`GET /api/jobs/{id}`、`cancel`
- [x] 代码：`Config` 支持 `$CREDENTIALS_DIRECTORY` 回退
- [x] 代码：`DICTATION_API_TOKEN` 可选鉴权（常量时间比较，鉴权先于 body 解析）
- [x] `install.sh`：`EFFECTIVE_BASE` 以 `config.toml` 为准，修 unit/config 不一致
- [x] 全量验证：fmt / clippy / 143 tests / JS / bash -n / x86 + riscv64 构建 / E2E curl
- [x] commit + push + tag `v0.1.0-rc.2` 并触发 Action（`5479cd7`，run `42196618526`）
- [x] 从 rc.2 release 再验一次 x86/RISC-V 资产（checksum、ELF、`--data-root` 安装、HTTP 生成）
- [x] 在目标 SBC 上真实安装 + systemd 开机自启 + 手机访问 + 服务端生成
      （已验收；设备上运行 v0.1.0 系列）

### 后续（S1 未覆盖）

- [x] 结果查询 HTTP 化：`GET /api/results`、`GET /api/results/{id}/wrong`
      （agent 不再需要主机侧 CLI 读 SQLite）
- [ ] 逐词生成进度（给 `RenderOptions` 加回调）
- [ ] 生成任务持久化（当前仅内存，重启后丢失；靠 cache/manifest 幂等恢复）
- [ ] `install.sh` 增加 `/usr/local/bin` symlink（仅本机 CLI 便利，agent 不需要）

---

## 3. F1 验收（词表 → 音频）

- [x] Cargo 项目可构建，`cargo test` 通过。
- [x] 纯文本解析 + `lesson.toml` + 行内属性。
- [x] `plan` 输出逐条时长与总时长，不调 TTS。
- [x] `mock` provider。
- [x] 豆包 V3 `seed-tts-2.0`（PCM 24k、`speech_rate`）。
- [x] Qwen `qwen3-tts-flash`（rate 后处理）。
- [x] VAD 裁剪、RMS 响度均衡、重采样、精确静音拼接。
- [x] `manifest.json` + `full.wav` + 可选 `full.mp3`。
- [x] 内容哈希缓存，二次生成 0 次 TTS 调用。
- [x] 开场白/结束语按语言自动选择并缓存。
- [x] 中英文响度均衡（实测 RMS 0.1122 / -19 dBFS）。
- [x] 豆包 V3 真实 smoke；两条 demo：
  `demo/smoke-zh.mp3`（57.96s，Vivi）、`demo/smoke-en.mp3`（59.29s，Stokie）。
- [ ] Qwen 真实网络 smoke：暂不使用（火山有赠送额度）；代码保留但未验证。
- [x] `dictation clean --unused/--older-than`（默认 dry-run，`--apply` 才删）。

## 4. F2/F3/F4 验收（播放器 + 自检 + 复习）

- [x] axum 静态服务 + 课时列表 + SQLite。
- [x] 播放/暂停、重听当前词、进度、Wake Lock、再听一遍（从头）。
- [x] 完成页：检查 / 再听一遍 / 返回首页。
- [x] 检查页：所有词、每行单听、只标错、未标默认正确。
- [x] 提交落库：attempts + review_state。
- [x] 错词当天可见：`review_count = 未纠正错词 + 到期词`。
- [x] 首页“错词复习”入口；复习标对后入口消失。
- [x] 自动混入下一课：最多 5 个，追加在本课之后、outro 之前，不加提示词。
- [x] 检查页「复习」标签；320/375/390px 实测不被挤压，长列表不被底栏遮挡。
- [x] 浏览器端到端：
  A 课标错「粗壮」→ B 课 3 项（窗户/操场/粗壮复习）→ 标对 → 入口消失。
- [x] `result list/show/wrong` 查询 SQLite；`wrong --latest --json` 输出错词 +
  来源课 + lapses/interval/due，供大模型分析。
- [x] 手机/平板实机：听音、暂停/重听、检查标错正常。
- [ ] 家长模式调 gap/rate/repeat。
- [x] 断点续听：返回首页保留进度，7 天内可继续；完成/提交后清除。
- [ ] `review_cap` 可配置（当前固定 5）。
- [ ] 错词本 / 答案纸打印。
- [ ] FSRS（当前 1/2/4/7/15/30 天简化间隔）。

## 5. F5-F8 计划

### F5 家长模式 / 答案纸

- 长按标题 + PIN 进入。
- 可调 gap / rate / repeat / review_cap；改 rate 触发按需重合成。
- A4 纵向、一列 20 题、题号 + 书写格、不印拼音。
- 同版式用于拍照批改对齐。

### F6 拍照批改

- 一张整页照片；Prompt 带期望词表 + 题号。
- JSON：`correct | wrong | missing | unclear` + `wrong_chars` + `confidence`。
- `confidence < 0.75` 或 `unclear` 转人工。
- 模型：`deepseek-flash`（原生多模态）或 Qwen-VL；`Grader` trait 隔离。
- AI 只做预填，家长确认是最终裁决。
- 图片默认本地存；上传云端开关。

### F7 SBC 部署 / 本地 TTS

- target：`riscv64gc-unknown-linux-gnu`（Linux（glibc ≥2.30））。
- 优先 `cargo zigbuild` 本地交叉编译；systemd 常驻。
- 先审计 `-sys` 依赖；`rusqlite(bundled)` 引入 `libsqlite3-sys`，需验证
  zigbuild 能否编译；失败则评估 `redb` / JSON 等纯 Rust 存储。
- 本地 TTS 可选：端侧 Qwen3-TTS-0.6B（SBC 未官方验收，需实测）或
  sherpa-onnx + VITS；云端 TTS 仍是日常首选。
- 断网降级。
- 状态：SBC 安装 + systemd 自启 + 手机访问 + 服务端生成已验收；
  本地 TTS 仍未做。

### F8 外网分享（Cloudflare Pages + R2，进行中：前端完成）

目标：课时发布到 CF，非家庭网络也能给孩子听写；结果导出后回家导入，形成复习闭环。
详细契约见 `docs/share-publish.md`（**唯一契约源**，本节只是摘要）。

**架构与边界**

- 双通道：Pages 只放壳（`web/` + 生成的 `config.js` + `_headers`）；R2 只放数据。
  发布新课时不重新部署 Pages；R2 是唯一真相源，本地不留发布账本。
- 只读发布，不做 R2 写接口、不做多租户；v1 不加访问控制（口令只留出口：
  前端统一走 `dataUrl()` 生成 R2 URL，`base` 保持干净，见契约 §8.5/§11.5）。
- 静态站不包含复习词（`review` 恒 false），复习仍靠本地 SQLite。

**发布物**（`<dataBase>/`）

- `lessons/index.json`：`{schema_version,updated_at,lessons[{id,title,subject,grade,term,item_count,total_ms,path,content_hash,published_at}]}`，派生缓存，可重建；`dir` 不发布。
- `lessons/<id>/lesson.json`：与 `SessionManifest` 同形；新增 `schema_version`/`lesson_id`/`content_hash`/`subject`/`grade`/`term`，省略 `session_id` 与 `base`；`full_track` 仅 `--with-full` 时出现。
- `lessons/<id>/clips/*.wav`：沿用内容哈希命名；**不传 `full.wav`**（约 9.8MB/课）。
- `<id>` = `sha256(目录名)` 前 12 位，与内容无关，重发只覆盖同一前缀。
- `content_hash` = 规范化 JSON 的 sha256（键排序、无空白；排除 `generated_at`/`total_ms`/自身）。

**发布流程（增量、幂等、只增不删）**

1. 解析课时 → 校验 manifest 与 clips 完整（缺 clip 直接报错，不发布残缺课）
2. GET 远端 `lesson.json`；hash 相同 → `skipped`（除非 `--force`）
3. 逐 clip `HEAD`，缺失才 `PUT`（`audio/wav`, `Cache-Control: immutable`）
4. `PUT lesson.json` = 提交点（`no-cache`）
5. GET → merge → `PUT index.json`（最后一步）
   - 顺序不可颠倒；失败只需重跑（幂等），错误带 `stage/uploaded/committed`
- 永不删除：重发/改名只新增 key；`unpublish` 只摘 index 条目，`--purge --apply` 才删对象
- 串行：进程内 Mutex + `publish.lock`，不依赖 R2 条件写
- 校验与修复：`--status` 报告 `missing`/`orphan`；`--reindex` 从 R2 重建 index（不读本地）

**配置与凭据**

- `config.toml` 新增 `[publish]`：`enabled`/`data_base`/`pages_base`/`bucket`/`pages_project`/`shell_dir`；
  未配置 → `503 {"error":"未配置发布目标（config.toml [publish]）"}`，不影响本地播放/生成。
- `/etc/dictation/env`（0600）：`R2_ACCOUNT_ID`/`R2_ACCESS_KEY_ID`/`R2_SECRET_ACCESS_KEY`（可选 `R2_BUCKET`/`R2_DATA_BASE`）。
- 沿用 D13b：`$CREDENTIALS_DIRECTORY` → 环境变量回退；agent 不接触凭证，只调 HTTP。

**静态前端（✅ 已实现，浏览器实测）**

- `web/config.js`（默认 `{mode:'server'}`）在 `app.js` 之前加载；发布时生成
  `{mode:'static', dataBase:...}` 覆盖。缺失/非法一律按 server，不隐式降级。
- `API` 拆成 `serverApi` / `staticApi`：static 只读 R2（`index.json` +
  `lesson.json`，补 `session_id`/`base`），`check()` 本地判分；
  review/深链/重掷入口在渲染阶段就不创建，**不发任何 `/api/*` 请求**（已实测）。
- 能力门禁集中在 `CAP`（review / lessonReview / regenerate / sessionLink）。
- 深链：static 用 `?l=<lesson_id>`，server 保留 `?s=<session_id>`；static 忽略 `s`。
- 保留：时间轴、上一个/下一个、重听、断点续听、听写节奏（隐藏面板）。

**结果回传**

- 导出 `dictation.result/v1`（✅ 已实现，含 `subject`；`navigator.share` → Blob 下载 → 文本框复制 三级降级；微信 UA 直接走文本）。
- `POST /api/results/import`（未实现）：幂等键 = 规范化
  `{content_hash,subject,finished_at,student,items[position/text/correct]}` 的 sha256，忽略客户端 id；
  已导入 → `duplicate`；时间基准用 `finished_at`（未来 >5 分钟钳制为 now）；单事务写
  `sessions(kind=imported)` + `session_items` + `attempts(source=import)` + `review_state`。
- CLI 等价（未实现）：`dictation result import <file|-> --json`。

**CLI（agent-first）**

```bash
dictation publish <课时> [--dry-run] [--force] [--with-full] [--json]
dictation publish --all | --shell | --status | --reindex [--json]
dictation unpublish <课时> [--purge --apply] [--json]
dictation result import <file|-> [--json]
```

**CF 侧一次性配置**

- Pages 项目 + R2 自定义域（**关闭 `r2.dev` 公开域名**）
- R2 CORS：仅 `GET`，`AllowedOrigins` = Pages 域名（`<audio>` 不需要，`fetch(lesson.json)` 需要）
- 缓存：clips `immutable`、`lesson.json`/`index.json` `no-cache`；可选 Cache Rule 缓存 `/clips/`
- Pages `_headers`：`Referrer-Policy: no-referrer`，壳资源短缓存
- 额度：100 课 ≈ 145MB、发布 2400 次 A 类、40 人×30 天 ≈ 2.6 万 B 类，免费版余量充足

**实施时必须同步**

- `skills/dictation-prep/SKILL.md`：流程加“发布 → 返回分享链接”；说明不碰 R2 凭证；503 不重试、404 先导入/生成、重复发布 `skipped` 不算错
- `AGENTS.md` 加一条设计决策（壳/数据双通道，R2 为真相源）
- `README.md` / `INSTALL.md` 补 CF 一次性配置步骤

**待定（实施时确认）**

1. 发布锁文件位置（`<data>/publish.lock` vs 系统 tmp）
2. `publish --shell` 依赖本机 `wrangler` 还是走 Cloudflare API（Direct Upload 签名复杂度）
3. R2 凭证是否复用 `/etc/dictation/env`、是否用 `LoadCredential`
4. 「导入结果」入口放首页还是独立页
5. `index.json` 是否记录 `with_full`/音频版本

---

## 6. 验证记录

### 6.1 自动化

- `cargo test`：188 passed（179 lib + 9 bin，截至 2026-11-09）。
- `cargo clippy --all-targets -- -D warnings`：0 warning。
- `cargo fmt --check`：通过。
- `cargo build --release`：通过。
- `node --check web/app.js`：通过。
- `web/manifest.webmanifest`：合法 JSON。

### 6.2 真实 TTS

- 豆包 V3 `seed-tts-2.0`：
  - 中文 `zh_female_vv_uranus_bigtts`（Vivi 2.0）
  - 英文 `en_female_stokie_uranus_bigtts`（Stokie）
  - 两条 demo 各含 intro + 5 词 + outro；中英文 RMS 0.1122。
- 修复过的问题：
  - 首尾静音不一致 → VAD 裁剪 + 固定静音。
  - 中英文响度差 3.8 dB → RMS 归一到 -19 dBFS。
  - Dacey 孤立英文词句尾上扬 → 换 Stokie。
  - 豆包 V3 音频块独立 padding 的 base64 → 逐块解码再拼 PCM；
    否则「毕竟」会出现块边界爆音。
- Qwen 暂不使用；代码保留但未验证（当前用火山豆包）。

### 6.3 浏览器

- 桌面 Chrome / 移动视口 390×844：
  - 首页 → 播放/暂停/重听 → 完成 → 检查标错 → 提交 → 复习入口 → 标对 → 入口消失。
  - 自动混入：A 课标错 → B 课出现「复习」词 → 标对后消失。
  - 320/375/390px 检查页标签和底栏不遮挡。
- 真实 20 词课时播放/暂停/重听正常。

### 6.4 实机

- 用户在自己的手机/平板 验证：听音、暂停/重听、检查标错正常。
- SBC 实机：安装、systemd 开机自启、局域网访问、HTTP 导入 + 生成全部正常。

---

## 7. 工作日志

### 2026-10-20

- 完成方案设计与需求确认。
- 设备：家用 SBC；手机/平板。
- 纸笔听写；允许云端批改；复习量默认 5、可调。
- P0 同时实现豆包 + 百炼 Qwen。

### 2026-10-20（P0）

- `lesson` / `plan` / `audio` / `cache` / `render` / `manifest` / CLI。
- `doubao` / `qwen` / `mock` 三个 provider。
- 52 tests；20 词 mock：40 片段 / 191.92s；二次生成 0 TTS / 40 cache。
- 未验证真实 TTS（当时无 key）。

### 2026-10-20（豆包 V3）

- 控制台只提供 API Key；从 V1 `appid/token` 切到 V3 `seed-tts-2.0`。
- Header：`X-Api-Key` + `X-Api-Resource-Id` + `X-Api-Request-Id`。
- 语速 `speech_rate` int：0=正常，100=2.0x，-50=0.5x。
- `PIPELINE_VERSION` 提高，旧缓存失效。

### 2026-10-20（真实 smoke / 开场白 / 响度）

- 豆包 V3 5 词 smoke 成功；中文 Vivi、英文最初 Dacey。
- 去掉逐题题号；加开场白/结束语（中英自动、缓存）。
- 中英文 RMS 差 3.8 dB → 改为 -19 dBFS RMS 均衡。
- 英文 Dacey 孤立词上扬 → 对比 Stokie/Tim/Dacey+逗号，选定 Stokie。

### 2026-10-20（「毕竟」怪声）

- 根因：V3 每个 `data` 块独立 base64 padding；先拼字符串再解码会静默错位。
- 修复：逐块解码再拼 PCM；仅当块内是半个 quantum 才回退拼接。
- 新增回归测试；`PIPELINE_VERSION` 升到 5。

### 2026-10-20（播放器 v1）

- axum + SQLite；课时/session/check/review/media/static。
- 儿童端五态：首页/播放/完成/检查/结果。
- 浏览器全流程；20 词演示课时。
- 93 tests（当时）。

### 2026-10-22（实机 + 自动混入）

- 用户在手机/平板 验证听音与标记。
- 自动混入：本课之后追加最多 5 个待复习词，`review=true`，base 指回原课。
- 检查页「复习」标签；320px 底部留白修正。
- 端到端：A 课标错 → B 课混入 → 标对 → 入口消失。
- 99 tests。

### 2026-10-22（文档拆分 + 安装包）

- `AGENTS.md` 收缩为稳定设计；进度/验收/日志迁到 `TODO.md`。
- `AGENTS.md` 增加依赖约束：优先纯 Rust、尽量避免 `*-sys`、用 `cargo zigbuild` 验证。
- 新增 `INSTALL.md`、`deploy/dictation.service`、`deploy/env.example`。
- 实机安装步骤覆盖：本机构建、SBC 交叉编译、复制清单、systemd、课时同步、
  agent skill 安装、升级/备份/排障/安全。
- 权限布局：`/opt/dictation` 只读程序；`/srv/dictation/{lessons,data,cache}` 可写数据；
  agent 用户加入 `dictation` 组，只写 `lessons/cache`，`data/` 由服务独占。
- skill 支持 `DICTATION_LESSONS_DIR` / `DICTATION_CACHE_DIR`，SBC 上可直接生成到
  `/srv/dictation/lessons`。

### 2026-10-22（config.toml + agent CLI 导入）

- 新增 `/etc/dictation/config.toml`（`deploy/config.example`）：`[paths]`
  base/lessons/data/cache/web + `[server]` host/port；`--config` / `DICTATION_CONFIG`
  可覆盖；`dictation config show --json`。
- 新增 agent 专用 CLI：
  - `dictation lesson import --payload - --json`：stdin JSON，
    校验后写 `lesson.txt`/`lesson.toml`，返回实际目录/词数/预计时长；
  - `lesson plan/gen/list --title ...`；
  - 路径限制在 `lessons_dir` 内，拒绝 `..`/绝对路径；`--force` 才覆盖。
- agent 不再直接读写 `/srv`、`/opt`；只把 payload 交给 CLI。
- `serve` 从 config 读 paths/server；systemd unit 改为
  `dictation --config /etc/dictation/config.toml serve`。
- skill 重写为 CLI 导入流程；INSTALL 增加 config/权限/agent 说明。
- `cargo test` 116 passed；CLI/serve/路径穿越/非法 TOML/重复导入实测通过。
- 新增代码后再次 `cargo zigbuild --release --target riscv64gc-unknown-linux-gnu`
  通过（8.64s），仍是 ELF64 RISC-V。

### 2026-10-22（缓存清理 CLI）

- 新增 `dictation clean`：
  - `--unused` 按课时 `clips/*-<hash8>.wav` 反查引用；
  - `--older-than 30d/12h/45m` 按 mtime；
  - 两者同给取交集；`.wav/.json` 成对删，孤儿也清；不跟随 symlink。
- **默认 dry-run**；必须 `--apply` 才删除；`--json` 输出
  `scanned/referenced/candidates/bytes/deleted/files/errors`。
- 真实 cache dry-run：276 个候选、约 6.6MB；未删除。
- 127 tests passed。
- skill 增加“清理缓存”流程：先 dry-run 报数，家长确认后 `--apply`。

### 2026-10-22（断点续听）

- PWA 保存 `{sessionId, manifest, index, savedAt}` 到 `localStorage`；
  每个 item 开始、暂停、pagehide/visibilitychange 时更新。
- 首页显示「继续上次听写」（标题 + 进度 + 时间），7 天过期；
  从保存的 item 继续，跳过 lead-in/intro。
- 完成、提交检查、清除、音频失败时清除；「不继续」二次确认。
- 浏览器实测：播放到 2/3 → 刷新 → 首页出现继续卡片 → 从 2/3 继续 →
  完成 → 首页卡片消失。
- 不引入 Service Worker；局域网常开即可。

### 2026-10-22（听写结果检索 CLI）

- 新增 `dictation result list/show/wrong`：
  - `list` 最近已提交的 session（`--all` 含未完成）；
  - `show` 整次逐项对错；
  - `wrong` 只导出错词，含 `origin_lesson`、`lapses`、`interval_days`、`due_at`。
- JSON 时间 RFC3339（+08:00）；错误统一 `{"error":...}` + exit 1。
- DB 只读查询，不修改 `data/`；DB 不存在时提示先运行 `serve`。
- 与 `serve` 同时打开同一 SQLite 的并发只读实测正常。
- `cargo test` 119 passed。
- skill 增加“查询听写结果 / 分析错词”流程：
  `result list` → `result wrong --latest --json` → 只基于返回的词做归纳，
  不编造错因，不直接改课时。

### 2026-11-06（v0.1.0-rc.1 已发布）

- GitHub Actions `Release` 全流程通过：
  Test 1m19s → Build 3m35s → Publish 10s。
- pre-release：
  `https://github.com/babydragon/dictation/releases/tag/v0.1.0-rc.1`
- 资产：
  - `dictation-v0.1.0-rc.1-x86_64-unknown-linux-gnu.tar.gz`（4.1MB）
  - `dictation-v0.1.0-rc.1-riscv64gc-unknown-linux-gnu.tar.gz`（3.7MB）
  - `install.sh`、`SHA256SUMS`
- `sha256sum -c` 两个资产均通过；RISC-V 资产确认为 ELF64 RISC-V。
- 从 GitHub release 用 `curl | bash -s -- --version v0.1.0-rc.1 --root ... --no-systemd`
  实测安装成功；二进制 `dictation 0.1.0-rc.1`，LICENSE/skill/deploy/web 均安装，
  权限 2775/0750/600 正确。
- 待办：在目标 SBC 上真实执行（systemd、开机自启、手机访问）。

### 2026-11-09（SBC 部署验收）

- 目标 SBC 上完成：安装、`systemd` 开机自启、手机/平板局域网访问。
- 服务端生成在设备上实测通过（HTTP 导入 → 生成 → 播放/自检）。
- agent 侧闭环已在真实 agent 环境验证，不再需要文件系统权限。
- 结论：v0.1.0 可作为家庭日常使用版本；离线本地 TTS 仍为可选增强。

### 2026-11-09（v0.1.0 正式版）

- 本地 agent 用 HTTP 闭环验证通过（导入 → 生成 → 播放/自检）。
- `Cargo.toml` / `Cargo.lock` → `0.1.0`；README/INSTALL/install.sh 示例同步。
- 提交 `ca5a6f4`，tag `v0.1.0`；Action `42557388437` 全流程通过，`prerelease=false`。
- 资产：x86_64（4.4MB）、riscv64gc（3.9MB）、`install.sh`、`SHA256SUMS`；
  `sha256sum -c` 通过，RISC-V 为 ELF64 RISC-V。
- `releases/latest` 已指向 `v0.1.0`，`latest/download/install.sh` 可用。
- 从 release 实测：`--root ... --data-root /var/lib/dictation` 安装成功
  （base 与 ReadWritePaths 一致），安装后二进制跑通导入 → 生成 → done。

### 2026-11-09（结果查询 HTTP 化）

- 契约先行：`docs/http-api.md` 新增「结果查询」段（5 个 GET 端点、
  查询参数、错误码、与 CLI JSON 一致性的实现约定），测试计划同步。
- 测试先行：`src/server/mod.rs` 新增 5 个 router 测试（空库、列表+详情+latest、
  `all=true`、非法参数/未知 id、token 不保护 GET），共 142 lib tests。
- 新增 `src/result_view.rs`：`format_unix`/`session_json`/`item_json`/`wrong_json`
  + 3 个单测；`main.rs` 的 `result` 子命令改为调用它，删除重复实现。
- `src/server/mod.rs` 新增路由：
  - `GET /api/results?limit=20&all=false`（limit 1–100，非法 → 400，
    不静默夹取；无记录 → 200 空列表）
  - `GET /api/results/{id}` / `GET /api/results/latest`
  - `GET /api/results/{id}/wrong` / `GET /api/results/latest/wrong`
  - 未知 id 404；`latest` 无已提交记录 404；未提交 session 的 `/wrong` 409。
- 实测：把真实 `data/dictation.db` 复制到 `/tmp` 后起 `serve`，`/api/results`、
  `latest`、`latest/wrong` 返回 200；`limit=0` 400、未知 id 404；
  HTTP 输出与 `dictation result show|wrong --json` **逐字段完全相同**。
- 验证：`cargo fmt --check`、`cargo clippy --all-targets -D warnings`、
  `cargo test` 151 passed、`node --check web/app.js` 均通过。

### 2026-11-09（CLI/HTTP 一致性）

- 契约：`docs/http-api.md` 新增「CLI 与 HTTP 的一致性约定」；`GET /api/lessons`
  改为返回 `id` + `dir`，排序 `title → dir`，与 CLI 同一实现。
- 新增 `src/lesson/list.rs`：`scan`（唯一文件系统读取）+ `json`（唯一序列化），
  3 个单测（排序、跳过坏 manifest、空目录、固定字段集合）。
- `db::scan_lessons` 改为调用 `lesson::list::scan`，只保留 upsert；
  `LessonEntry` 增 `dir_name`（相对目录名），`dir` 仍是绝对路径且不外发。
- `GET /api/lessons` 与 `dictation lesson list --json` 字段统一为
  `{id, dir, title, item_count, total_ms}`；新增 2 个 parity 测试。
- `AGENTS.md` §16 增加强制约定：同一能力两侧共用实现；运维类操作新增/修改必须
  两侧同步、语义一致。§13 目录补 `lesson/list.rs`、`result_view.rs`。
- skill 的课时列表说明补齐 `dir` 字段与用途。
- 验证：fmt / clippy / 156 tests / `node --check` 通过。

### 2026-11-10（v0.2.0-rc.1 已发布）

- 提交 `f948c49`，tag `v0.2.0-rc.1`；Action `42734981595` 全流程通过。
- pre-release：`https://github.com/babydragon/dictation/releases/tag/v0.2.0-rc.1`
- 资产：x86_64（4.7MB）、riscv64gc（4.1MB）、`install.sh`、`SHA256SUMS`；
  `sha256sum -c` 通过，RISC-V 为 ELF64 RISC-V。
- 从 release 实测：`install.sh --root ... --no-systemd --version v0.2.0-rc.1`
  安装成功，二进制 `0.2.0-rc.1`。
- release 二进制搭真实 v1 数据库副本启动：自动迁移到 `user_version=3`、
  生成 `dictation.db.bak-v1-*`、科目回填正确；HTTP 导入（english/grade 3）→
  mock 生成 → `POST /api/sessions` 返回带 subject/grade 的 manifest。
- 下一步：设备上验证（科目/复习组卷/错词复习/范围复习），再决定 `v0.2.0` 正式版。



- `GET /api/review/queue`：返回可播放的待复习词（错词→最久未复习排序），
  支持 `limit`(1–200)、`grade`/`term`；`total` 不受 limit 截断。
- `POST /api/sessions/custom`：按 agent 给定的词表与顺序创建临时复习会话；
  1–60 个词，重复/未知/缺音频分别 400；不建课时目录、不调 TTS。
- `GET /api/sessions/{id}`：重放已创建会话；课时会话重新读 manifest，
  复习/自定义 session 用复习默认值。
- schema v3：`session_items` 增加 `repeat_gap_ms`，`GET /api/sessions/{id}` 可精确重放
  （否则非默认 repeat_gap 的课时会按 1200ms 播放）。
- PWA 支持 `?s=<session_id>` 深链：首页出现「点击开始」卡片（规避 iOS 自动播放限制）。
- skill 新增「错词复习 / 范围复习」流程，写明 60 词上限、错误处理、agent 只选词不排期。
- 验证：188 tests；fmt / clippy / JS 通过。

### 2026-11-18（复习第二轮设计：课内复习 + 范围复习，仅文档）

- 讨论结论：英语入门单元只有 ~10 词，自动混入只有 3 个，需要能主动复习；
  但**不做**「复习计划」这类实体——范围只是请求参数，进度由 agent 每次重算。
- 孩子端只保留一个复习入口：**课内复习**，只取本课 `again`/到期词；
  词集合以 `manifest.json` 为准（不用 `words.clip_lesson`，它会被后来的课覆盖）。
- 「整课重排」「跨课范围复习」只给 agent / 家长端：`GET /api/words` 取词与统计
  （`lapses` / `correct_streak` / `wrong_recent`），`custom` + `shuffle` 组卷。
- 新增调度规则：未到期词答对**完全不写 `review_state`**；答错照常回退。
- 排序改用真实 `attempts.created_at`（schema v4 只加索引），否则提前练习不更新
  派生值，会反复抽到同一个词。
- 明确混入边界：只有 `POST /api/sessions` 混入，`kind='review'` 永不混入。
- 文档：`docs/review-scheduling.md` 增 §4.5 / §4.6 / §6.5 / §6.6、测试与决策；
  `docs/http-api.md` 增 `GET /api/words`、`GET /api/review/lessons`、
  `review.lesson_id`、`custom.shuffle`、提前复习调度、v4 索引。
- `AGENTS.md` §10 已同步：提前复习不动调度、孩子端课内复习、范围复习归 agent/家长端。
- 新增 `docs/glossary.md`：统一全部术语、代码标识、枚举值，并列出旧称/禁用词
  （加练 → 错词复习、AI 组卷 → 范围复习、课程 → 课时）。
- `AGENTS.md` 增加「术语先登记，再使用」约定（§16）与接手清单项（§19）；
  `http-api.md` / `player.md` / `review-scheduling.md` / `README.md` / skill 顶部
  都指向词表。
- 代码未动，因此没有跑 fmt/clippy/test。

### 2026-11-19（F8 前端：静态模式 + 结果导出）

- 契约：`docs/share-publish.md` 按当前实现对齐（科目进 `index.json`/`lesson.json`/
  `content_hash`/`result/v1`；API 能力表 10 项；`?l=`/`?s=` 双深链优先级；
  `dataUrl()` 唯一出口；口令决定「v1 不启用、只留出口」）。
- 模式开关：新增 `web/config.js`（默认 `{mode:'server'}`），
  `index.html` 在 `app.js` 前加载；缺失/非法一律按 server。
- `API` 拆 `serverApi` / `staticApi`：static 只读 R2（`index.json` → 课时列表、
  `lesson.json` → 补 `session_id`/`base`），`check()` 本地判分；
  review/复习角标/长按重掷/`?s=` 入口经 `CAP` 在渲染阶段裁掉。
- `dataUrl()`：静态数据源唯一出口，`base` 保持干净（不进断点续听快照）。
- 深链：static `?l=<lesson_id>`；`?s=` 在 static 下忽略；列表拉取失败时
  深链卡片仍显示。
- 导出：结果页新增「导出结果」（仅 static），`dictation.result/v1` 全字段
  （含 `subject`/`content_hash`/`url`）；三级降级 `navigator.share(files)` →
  Blob 下载 → 文本框复制（微信 UA 直接文本）。
- 浏览器实测（同源 fixture：壳 + R2 数据）：首页渲染科目标签、无复习入口；
  完整听写 → 检查标错 → 本地判分 4/1；上一个/下一个跳转正常；
  `?l=` 深链 + `?s=` 忽略；**服务端日志确认 0 次 `/api/*` 请求**；
  导出下载文件名 `快速测试课-20261119.json`、文本降级内容与契约逐字段一致。
- server 模式回归：`serve` 起本库，首页渲染与 `/api/*` 正常，导出按钮隐藏。
- 验证：`node --check web/app.js` 通过；Rust 未改动（cargo test 仍 200）。

### 2026-11-20（异步发布 + 准备阶段冻结控件）

- `POST /api/lessons/{id}/publish` 新增 `{"async": true}`：202 + job_id，
  后台执行，复用 `GET /api/jobs/{id}`（`kind="publish"`，result 与同步同形）；
  同课活动任务复用 job_id；生成/发布任务按 kind 隔离，互不影响。
  解决 agent 客户端 60s 超时（上行慢时同步发布会被取消）。
- `Job` 增加 `kind`，`result` 改为 JSON（生成结果字段不变）；
  `cancel_generation` 只取消生成任务。
- 前端整轨拼装阶段：冻结暂停/重听/上一个/下一个，状态显示
  「准备音频… N/M」（逐词进度），播完恢复；失败回退时也恢复。
- 验证：浏览器 MutationObserver 实测 1/20→20/20 期间三个按钮均 disabled，
  进入「正在听写」后恢复；异步发布 3 个 HTTP 测试（202+轮询、去重、kind 隔离）。
- 文档：`http-api.md`、`share-publish.md`、skill。

### 2026-11-20（B 方案：Pages Function 代理私有桶）

- 采用 B：R2 不绑公网域名、保持私有；Pages Function 通过 binding 读桶，
  以同源 `/data/*` 提供 JSON 与片段（无 CORS、只有一个公网域名）。
- 修正 Function 的 key 映射：`/data/lessons/...` 直接对应桶内 `lessons/...`
  （不能重复加 `lessons/` 前缀），并限定只读 `lessons/` 下的 key。
- `dictation publish --shell`：生成 `<shell_dir>`（默认 `dist/pages`）：
  `wrangler.jsonc`（binding `BUCKET`）、`functions/data/[[path]].js`、
  `public/`（静态壳 + 生成的 `config.js` + `_headers`）；重建时只清理
  `public/` 与 `functions/`；支持 `--dry-run`/`--json`。不需要 R2 密钥。
- `dataBase` 默认取 `pages_base + /data`；前端 `dataUrl()` 支持同源相对路径。
- 验证：生成工程结构/内容断言（4 个单测）；`cargo test` 236 lib + 9 bin、
  fmt/clippy 干净、`node --check` 通过。
- 文档：`share-publish.md`（架构/布局/§11 全部改为 B、§10 `--shell`）、
  `AGENTS.md` D16、`TODO.md`。
- Cloudflare 侧（2026-11-20）：建私有桶 `dictation`、Pages 项目 `dictation`、
  域名 `dictation.zimo.life`（CNAME 手工补，API 无 DNS 写权限）；`wrangler login`
  后部署壳，空桶下首页显示「还没有发布的课时」，Function 返回干净的 404
  （binding 生效）。
- 真机反馈修复：`.btn { display:inline-flex }` 覆盖了 `hidden` 属性，导致静态版
  仍显示「导入听写结果」→ 全局 `[hidden]{display:none!important}`；静态壳空索引
  404 视为空列表；`_headers` 对 shell 资源显式 `no-cache`。
- rc.8：版本升到 0.3.0-rc.8，回归（fmt/clippy/test/node/install.sh）通过。

### 2026-11-20（agent 发布 skill + rc.7 发布）

- `skills/dictation-prep/SKILL.md` 补「发布到分享站」：`POST /api/lessons/{id}/publish`、
  `dry_run`/`force`、成功/幂等响应、503/404/400/502 的处理约定；明确 agent 只调
  HTTP、Pages 壳部署是家长的一次性运维步骤。
- 实测端点连通：未配置 `[publish]` 时返回 `503 {"error":"未配置发布目标（config.toml [publish]）"}`。
- rc.7 打包：本地 fmt/clippy/test 通过后提交并推 `main` + tag `v0.3.0-rc.7`，
  GitHub Actions 成功产出 x86_64 / riscv64 资产与 `install.sh`、`SHA256SUMS`。
- 真机验收（家长）：Android Chrome / 微信 / 钉钉 WebView 熄屏均连续播放；
  iPhone 通过；锁屏暂停与页面状态同步已修。

### 2026-11-20（熄屏播放：整轨引擎 C1）

- 根因确认（`docs/playback-track.md`）：wake lock 在局域网 HTTP 不存在；时间轴建在
  JS 定时器上；`visibilitychange` 主动暂停。
- 新增整轨引擎（`web/app.js`）：
  - `assembleTrack()` 按 manifest 把片段 + 静音拼成一条 WAV（Blob），
    `readWavPcm()` 遍历 RIFF 并校验 `24k/mono/s16`；静音复用零缓冲；
    动态节奏沿用 `timingGapMs()` 的 −80ms 换算；
  - 独立 `<audio id="track">`（与 `#audio` 分开，`unlockAudio` 双解锁）；
  - 进度以 `currentTime` + `starts[]` 二分为准，`ended` 不派发时按「接近结尾」兜底；
  - 暂停/继续、上一个/下一个、重听全部改为 `seek`；`visibilitychange=hidden`
    在整轨模式**不暂停**；
  - 回退：片段 404 / 格式不合规 / >32MB / `play()` 失败 / 无 Blob 能力 →
    清理整轨并提示「改用逐词播放」；`?track=0` 强制分片引擎；
  - 长按重掷：只重建该 item 的 chunk、重算后续 `starts[]`、换 Blob 后 seek 回原位；
  - `?diag=1` 覆盖层：visibility 时间线、Blob 字节/时长、audio 时长、
    `currentTime`、回退状态（钉钉无 devtools 时取证）；
  - 锁屏/通知栏媒体卡片：`MediaSession.metadata`（标题 = 课时标题），按键交给系统。
- 真机反馈修复（2026-11-20）：
  - 从头播放时误 `seek(starts[0])` 跳过了 lead_in 与开场白 → 改为从 0 开始，仅续听 seek；
  - `?diag=1` 面板遮挡底部按钮 → 移到右上、`pointer-events:none`；
  - 锁屏/通知栏暂停绕过 `togglePause` → 监听 `track` 原生 `play/pause` 同步
    `player.paused`/UI/断点/媒体状态（自己操作元素时先改状态，事件里跳过）。
- 验证：20 词课 `trackMs=200409` vs `manifest.total_ms=200410`（差 1ms）、9.6MB；
  暂停/继续、上一个/下一个、重听正常；静态 fixture 216KB/4.5s 播完、检查页正常；
  `?track=0` 回退正常。**真机熄屏 T1/T2、钉钉/微信 WebView、重掷补丁待实测。**
- 跨端契约：`share-publish.md` §11.2 CORS 改为 **JSON + `clips/*`**（播放器用
  `fetch` 读片段）；R2 配置需同步，否则分享版点开始会失败。
- 文档：`AGENTS.md` D16、`player.md` 双引擎、`glossary.md` 整轨/拼装/引擎、
  `playback-track.md` 置为已实施、`TODO.md`。

### 2026-11-19（F8：结果导入闭环）

- schema **v5**：新增 `result_imports(import_key PRIMARY KEY, session_id, created_at)`
  作为导入幂等键；迁移只加表，旧库自动升级。
- 新增 `src/result_import.rs`：`dictation.result/v1` 解析/校验/幂等键。
  - 校验：schema 精确匹配、subject ∈ `chinese|english`、items 1–200、
    position 唯一且从 1 起、text 去空白后非空且 ≤200 字符、lang 缺省按文本推断；
  - 幂等键 = 规范化 `{content_hash, subject, finished_at(Unix), student,
    items[position,text,correct]}` 的 sha256，**忽略客户端 id**；
  - `finished_at` 必须 RFC3339；未来 >5 分钟钳制为当前时刻。
- `Db::import_result`（单事务）：`sessions.kind='imported'` +
  `session_items` + `attempts(source='import')` + `review_state` +
  `result_imports`；**时间基准是 `finished_at`**（不是导入时刻）。
  - 同一 `subject + finished_at` 但内容不同 → 409（疑似改判后重复导出）；
  - 重复导入 → `duplicate`，不重复写 attempts/lapses；
  - 词按 `(subject, text)` 归属；音源解析优先级：本机 `words` → 本机课时清单
    （`ClipIndex::scan`，同科目）→ 留空（历史可查但不进复习池）；
  - 排期逻辑从 `check_session` 抽出 `schedule_after_grade`，两处共用。
- HTTP：`POST /api/results/import`（鉴权优先、400/409/200）；CLI：
  `dictation result import <file|-> [--json]`，输出与 HTTP 同形。
- 前端（家长入口，孩子不可见）：长按首页标题 →「听写节奏」→「导入听写结果」，
  支持选文件与粘贴 JSON；成功/重复/失败就地提示，并刷新首页复习入口。
- 测试：+8 result_import 单测（含 ClipIndex）、+5 db 导入测试（幂等、按
  finished_at 排期、无音频不进池、时间冲突、清单补音源）、+4 HTTP 测试。
- 端到端（serve + 浏览器）：家长面板粘贴结果 →「已导入：1 对 / 1 错；待复习 1 个词」，
  首页立即出现「错词复习 · 语文」与该课的「复习 1」；再次导入 →「之前已经导入过，
  没有重复计分」；CLI `result import` 同样返回 imported/duplicate，`result show`
  可见 `source=import`、`origin_lesson`。
- 文档：`http-api.md`（端点 + 字段表 + 示例、v5 表）、`share-publish.md` §9、
  `glossary.md`（imported/import 转正、导入结果）、`AGENTS.md`（v5 + 端点 +
  导入排期基准）、`player.md`（断点校验，上一轮）。

### 2026-11-19（断点续听：删除/重建课时的进度校验）

- 问题：课时删除后「继续上次」卡片仍在（localStorage 不校验课时存在）；
  重建后本地 manifest 可能是旧词表。删除保留 `sessions`，`?s=` 深链也能
  对"死课"渲染卡片（音频 404 才暴露）。
- 修复（`web/app.js`）：
  - `pruneStaleResume()`：首页渲染（课时列表加载成功后）按 resume 的课时 id
    校验——不存在 → `clearResume()` + toast「原课时已删除，进度已清除」；
  - `resumePlayback()`：server 续播**不再重放本地 manifest**，改为对当前课时
    新建会话（`POST /api/sessions`）并按保存的词序号跳转（越界夹取）——重建后
    天然用新词表；static 校验 `content_hash` 与 `index.json` 一致，不一致 →
    清除 + toast「课时已更新，进度已重置」；
  - `?s=` 深链卡片渲染前校验课时仍存在，否则不渲染并提示下线。
  - 离线（列表加载失败）不校验，保留卡片；复习/自定义会话（无课时定位）不校验。
- 浏览器实测（static fixture + serve 真库）：
  - 删除课时 → 卡片消失 + localStorage 已清（server 与 static 各验一次）；
  - static 改 `content_hash` → 卡片消失 + 进度清除；
  - hash 未变 → 卡片正常，续播词序号正确（static 直接续播、server 新会话 2/20）。
- 文档：`player.md` 断点续听补校验规则；`share-publish.md` §8 同步。

### 2026-11-19（F8 后端：publish）

- 新增 `src/publish.rs`：
  - `PublishSettings`（`[publish]` 段 + `R2_*` 环境变量，密钥走 `read_secret`，
    未配置清单 `missing()`）；
  - `ObjectStore` trait + `S3Store`（SigV4 自签名，新增纯 Rust `hmac` 依赖，
    GET/HEAD/PUT）+ `MemoryStore`（测试）；SigV4 用 Python 独立实现对照出
    已知答案向量（GET/PUT 各一条）——期间发现参考实现漏了 `AWS4` 前缀、
    Rust 侧 HMAC 链误用同一实例 `update`，两处都已修正；
  - 载荷构造 `build_plan`（lesson.json + 对象清单，缺片段直接报错不发布）、
    `content_hash`（规范化 JSON sha256，排除 content_hash 自身）、
    `merge_index`（未知条目保留、按 title 排序）；
  - 状态机 `publish_lesson`（§5.1）：GET 远端 → hash 相同 skipped →
    逐 clip HEAD/PUT → lesson.json（提交点）→ index.json merge；
    错误带 `stage/uploaded/committed`（local/clips/commit/index）。
- 配置：`config.toml` 新增 `[publish]`（`enabled/data_base/pages_base/bucket/
  pages_project/shell_dir`），`AppConfig.publish` 贯通。
- CLI：`dictation publish [课时] [--dry-run|--force|--with-full|--json]`；
  省略课时 = 发布全部；未配置 → `{"error":…}` + exit 1（已实测）。
- HTTP：`POST /api/lessons/{id}/publish`（同步、token 优先于 body、
  503/404/400/502）；`docs/http-api.md` 补字段类型表与示例；鉴权清单同步。
- 测试：+13（SigV4 已知答案 ×2、URI 编码、载荷形状、缺片段、index 合并、
  首发跳过/force、远端孤儿保留、缺片段零写入、dry-run、HTTP 503/404/流程/
  token/不发布私有文件）；`cargo test` 216 lib + 9 bin，fmt/clippy 干净。
- 未实现（记录在 share-publish.md §15）：`publish.lock`、`--shell`、
  `--status`、`--reindex`、`unpublish`、真实 R2 冒烟。

### 2026-11-19（接口示例规范）

- skill 的「清理重复课时」补真实请求/响应示例与**字段类型表**
  （`files` 是整数、`words_repointed` 是对象数组、`words_cleared` 是字符串数组）。
- skill 的 `regenerate-word` 补请求/响应示例（返回 job，需要轮询）。
- `AGENTS.md §16` 新增：**每个接口都必须有请求示例 + 真实响应示例**，
  新增/修改字段同提交更新示例；调用方不得靠字段名猜结构。
- `docs/http-api.md` 开头补「示例与字段类型」约定。
- 起因：外部 agent 只看到字段名，自行编了 mock 结构（把 `words_repointed` 当成
  字符串数组），真实返回是对象数组导致崩溃。

### 2026-11-19（删除课时）

- 新增 `src/lesson/delete.rs`：`plan` 生成可审阅的删除计划，`apply` 执行；
  CLI `lesson delete`（默认 dry-run）与 `POST /api/lessons/{id}/delete`
  （`{"apply":true}` 才执行）共用同一实现。
- 语义：保留 `sessions`/`attempts`/`review_state`；`words.clip_lesson` 指向被删课时的词，
  自动重指向仍包含该词的同科目课时，否则清空 clip（退出复习池）。
- 新增 `lesson duplicates` / `GET /api/lessons/duplicates`，按科目+标题分组列出重复项。
- 安全：拒绝 `lessons/` 之外的路径；先 DB 后删目录，失败可重跑。
- 实测（临时目录 + HTTP）：两课同标题，旧课有 1 节课历史；dry-run 报
  `words_repointed=[甲→新课]`、`words_cleared=[乙]`、files/bytes/sessions；
  apply 后旧目录消失、新课保留、`results` 仍能查到历史会话、甲可播放、乙退出复习池。

### 2026-11-19（长按兼容与播放页重掷）

- 播放页长按「重听这个词」= 重掷当前词；前端轮询 `/api/jobs/{id}` 到 `done` 再提示。
- 重掷后重听给 URL 加 `?v=<时间戳>`，绕过浏览器对同名片段的内存缓存。
- Android 选字手势：`html,body` 全局 `user-select: none` +
  `selectstart` 捕获拦截（输入框例外）；微信 `contextmenu` 也当作长按
  （与 pointer 计时 800ms 去重）。
- `regenerate-word` 前置校验 `lesson.txt`，缺失返回 400 而不是任务报错。
- 验证：CDP 鼠标/触摸两条路径、`?v=` 实际出现在资源请求里；
  `cargo test` 200 passed、fmt/clippy、`node --check`。

### 2026-11-19（播放导航 / 隐藏节奏面板 / 单词重新生成）

- 播放页新增「上一个 / 下一个」：独立一行、56px；`jumpTo` 重开时间轴，
  暂停态跳转保持暂停，首项/末项置灰，断点续听 index 同步。
- 隐藏「听写节奏」：长按首页标题 1.5s，输入**词后等待秒数**（0–120）；
  换算 `gap_ms = round(秒×1000) − 80`（片段 20ms 前导 + 60ms 尾音），
  只覆盖词后等待；存 `localStorage["dictation.timing.v1"]`，纯客户端，
  静态发布同样可用。实测：默认 6s 与设 10s 的「点开始→第 2 项」差 3.92s
  （9.92 − 6.00），换算正确。
- 单词重新生成：`POST /api/lessons/{id}/regenerate-word` 按 `meta.text` 删缓存样本，
  再 `force` 重渲染（其它词命中缓存，1 次 TTS，片段文件名不变）；
  检查页长按词文本 1.5s 触发，仅课时会话。
- 触摸兼容：CDP 触摸事件验证长按、滑动取消、保存、确认重生成全流程；
  修掉「长按松手补发的 click 把确认框点掉」，改为吞掉紧邻的一次 click（400ms 失效）。
- 验证：`cargo test` 200 passed、fmt/clippy、`node --check`；两处长按在
  320/390px 下无溢出。
- 尚未真机验证：iOS Safari 的系统长按菜单抑制与滚动惯性下的 `pointercancel`。

### 2026-11-18（复习第二轮实现：课内复习 + 范围复习）

后端：

- schema **v4**：只加 `idx_attempts_word(word_id, created_at)`；`migrate_v3_to_v4` 幂等，
  旧库自动升级（v1 起可一路迁到 4）。
- 三处复习选词 SQL（混入、`/api/review/queue`、复习会话）统一用 `REVIEW_ORDER_BY`，
  排序键改为相关子查询 `MAX(attempts.created_at)`（走 `idx_attempts_word`），
  没有练习记录时回退到派生值。
- `check_session` 增加提前复习分支：`last_grade='again'` 或 `due_at<=now` 走原规则；
  否则答对**不写** `review_state`，答错照常重置 1 天 + `lapses+1`。
- `create_review_session_filtered` 抽出 `review_session_core(ReviewScope)`，
  新增 `create_lesson_review_session`：词集合来自 manifest，`grade/term` 只存不过滤。
- `POST /api/sessions/review` 新增 `lesson_id`：与 `grade`/`term` 互斥（400）、
  未知课时 404、科目不符 400、无可复习词 409；`title="<课时> · 复习"`。
- 新增 `GET /api/review/lessons`（`subject` 可选）：按 manifest 词集合 × `review_state`
  聚合 `review_count/due_count/wrong_count/learned_count`，只返回 `review_count>0`。
- 新增 `GET /api/words`：课时 manifest 并集、去重保序、`sources` 记录来源课；
  返回 `attempts/wrong/correct/correct_streak/wrong_recent/lapses/interval_days/due_at`；
  `limit` 1–500、`offset`、`total`；未知课时 400 + `unknown`。
- `create_custom_session` 增加 `shuffle`（session 种子洗牌，顺序落 `session_items`）。

前端：

- 课时卡从整块 `<button>` 改为 `div.card-lesson` + `.card-open`（`role=button`、
  Enter/Space 可进）+ 底部独立 `.card-review-btn`「复习 N」，不嵌套按钮。
- `loadHome` 并行拉 `/api/review/lessons`，按 `lesson_id` 挂 `review_count`；
  接口失败等价于没有复习词。
- 点击「复习 N」→ `POST /api/sessions/review{subject, lesson_id}` → 同一播放器；
  409 toast「这一课没有需要复习的词」并刷新首页。
- `?mock` 补 `/api/review/lessons`、`lesson_id` 复习、`<课时> · 复习` 标题。
- 320/360px 断点收紧内边距，复习条整行满宽、`min-height:56px`。

验证：

- `cargo test` **198 passed**（189 lib + 9 bin）；`fmt --check` / `clippy -D warnings` /
  `node --check web/app.js` 通过。
- 真实 `serve` + mock provider 端到端：导入生成 → 标错 → `/api/review/lessons`
  → 课内复习只返本课错词 → `/api/words` 统计与 `sources` → `shuffle`
  → 提前复习答对 `interval/due/lapses` 三字段不变，答错重置。
- 真实 DB + headless Chrome：Unit 1 卡片显示「复习 2」、Unit 2 无入口；
  点击后播放器标题「Unit 1 · 复习」、进度 1/2；320/390px 无横向溢出、
  DOM 中无嵌套 `<button>`。

### 2026-11-09（错词复习）

- `POST /api/sessions/review` 支持 `limit`（默认 20，范围 1–40）、`grade`（1–6）、
  `term`（1–2）；非法值 400。
- `create_review_session_filtered`：按 `words.clip_lesson → lessons.grade/term`
  过滤来源课；选词排序沿用「错词 → 最久未复习 → due_at」；session 记录
  subject/grade/term。
- 首页科目复习卡显示「最久 N 天未复习」，便于判断是否需要错词复习。
- 测试：db 的 limit/grade/term 过滤；HTTP 的 limit 生效、默认值与越界校验。
- 验证：182 tests；fmt / clippy / JS 通过。

### 2026-11-09（复习组卷算法）

- `review_cap` 写入 `manifest.json`（顶层，默认 8，0=关闭），服务端从 manifest 读取；
  修复此前「lesson.toml 配置不生效」的问题。
- 数量：`min(review_cap, max(3, ceil(新词数 × 25%)))`；20→5、30→8、40→8。
- 选词优先级：`last_grade='again'` 优先 → 距上次复习最久
  （`due_at - interval_days*86400`）→ `due_at` → id。
- 播放顺序：选词后按 session 随机种子 `sha256(seed + text)` 洗牌，
  同一 session 稳定、不同 session 不同，避免固定搭配形成位置记忆。
- 时长护栏：复习段累计音频 ≤ 150s；至少保留 1 个，避免静默空复习。
- 验证：180 tests（含优先级、比例、预算、洗牌稳定性、cap=0 集成）；
  fmt / clippy / JS 通过。

### 2026-11-09（科目隔离 + schema v2）

- 迁移框架：`PRAGMA user_version`（当前 v2）；迁移前 `VACUUM INTO` 备份、只留最近 3 份；
  库版本高于程序 → 拒绝启动；`words` 重建保留原 id，按「含拉丁字母 → `english`，
  其余 → `chinese`」回填。
- 数据模型：`subject`（`chinese`/`english`）必填；`grade`（1–6）/`term`（1/2）可选；
  词身份改为 `(subject, text)`；manifest、session、结果查询全链路带科目。
- 导入：HTTP 与 CLI 都要求 `subject`，`grade`/`term` 可选；与传入 toml 合并后写入
  `lesson.toml`（payload 覆盖同名键）。
- 复习：`POST /api/sessions/review` 必须带 `subject`；自动混入与
  `GET /api/review/summary` 的 `subjects` 分组都按科目隔离；首页按科目出卡。
- 兼容：旧 manifest 缺 `subject` 读作 `chinese`；旧库自动迁移；旧 PWA 需同版本更新
  （复习请求现在带 `subject`）。
- 验证：173 tests；真实 `data/dictation.db` 副本迁移成功（user_version=2、生成备份、
  词 id 保留）；HTTP 实测导入缺 subject 400、带 subject 201、复习缺 subject 400、
  科目分组正确。
- 下一步：组卷算法（`review_cap` 生效、优先级、数量、洗牌、150s 护栏）。

### 2026-11-09（文档按功能拆分）

- `docs/P1-CONTRACT.md` 拆成两份功能文档：
  - `docs/http-api.md`：服务端 HTTP API、SQLite schema、鉴权、CLI/HTTP 一致性约定；
  - `docs/player.md`：客户端播放器与自检交互（SessionManifest、时间轴、检查页、
    首页、断点续听）。
- 本地工作稿 `docs/P2-SHARE.md` → `docs/share-publish.md`（仍未入库）。
- 同步交叉引用：`AGENTS.md`、`README.md`、`INSTALL.md`、`TODO.md`、skill；
  源码注释去掉 P1/P2/P3 阶段标签。
- 顺手修正 `player.md` 中过期的「v1 不自动混入下一课」，与已实现行为对齐。

### 2026-11-06（v0.1.0-rc.2 已发布）

- 提交 `5479cd7`，tag `v0.1.0-rc.2`；Action `42196618526` 全流程通过
  （Test 1m20s → Build 3m26s → Publish 11s）。
- pre-release：`https://github.com/babydragon/dictation/releases/tag/v0.1.0-rc.2`
- 资产：x86_64（4.4MB）、riscv64gc（3.9MB）、`install.sh`、`SHA256SUMS`；
  `sha256sum -c` 通过，RISC-V 资产确认为 ELF64 RISC-V。
- 从 release 实测：`install.sh --root ... --data-root /var/lib/dictation`
  安装成功，`base=/var/lib/dictation`、`ReadWritePaths=/var/lib/dictation`；
  安装后的二进制用 mock 跑通 HTTP 导入 → 生成 → done。
- 注：首次 `git push origin v0.1.0-rc.2` 未触发 Action，删除远端 tag 重新推送后
  正常（GitHub 侧偶发，重推即可）。
- 待办：安装器结尾提示仍写“serve 不需要 TTS key”，已在本地修正，随下个版本发布。

### 2026-11-06（S1：agent 走 HTTP + 密钥收口）

背景：agent 的 fs 白名单很窄，`/usr/local/bin` 的软链接会被 `realpath` 解析后
拒绝；同时 TTS key 原本既不属于服务（旧 `serve` 不生成），也不属于 CLI
（`/etc/dictation/env` 是 0600 root）。改成服务端生成后两个问题一起消失。

- 文档先行：`docs/http-api.md` 新增「服务端生成」契约（4 端点、
  状态机、鉴权、并发、保留策略）；`AGENTS.md` D13/D13b、架构图、目录、
  env、开发约定同步；skill 全面改为 HTTP 流程；README/INSTALL/
  `deploy/env.example`/unit 同步。
- `src/lesson/import.rs`：把 CLI 的校验与落盘抽成 `lesson::import`，
  CLI 与 HTTP 共用；`ImportOutcome` 分离 `dir_name`（可外发）与 `dir_path`。
- `src/server/mod.rs`：`build_router()` 可无 socket 构建；新增
  `POST /api/lessons`、`POST /api/lessons/{id}/generate`、
  `GET /api/jobs/{id}`、`POST /api/lessons/{id}/generate/cancel`。
  内存 job 表 + 串行信号量（并发 1）+ 队列上限 4；
  `manifest.json` 比词表新则 `skipped`；同课进行中复用同一 job。
- 鉴权：`DICTATION_API_TOKEN` 可选；常量时间比较；**鉴权先于 body 解析**，
  坏 body 不再返回 axum 纯文本 415/400。
- `src/config.rs`：`read_secret` 优先读 `$CREDENTIALS_DIRECTORY/<NAME>`，
  回退环境变量；unit 里提供注释掉的 `LoadCredential` 用法。
- `install.sh`：`EFFECTIVE_BASE` 以已装 `config.toml` 的 `[paths].base` 为准，
  `--data-root` 只影响新装；unit `ReadWritePaths`、数据目录、`useradd --home`
  跟随配置；冲突时打印迁移步骤。
- 验证：143 tests（lib 134 + bin 9）、fmt/clippy、JS、`bash -n`、
  x86 与 riscv64 构建通过；用 release 二进制实测导入→生成→轮询→跳过、
  401 优先级、路径穿越 400、绝对路径不外泄。
- 已删除 `v0.1.0-rc.1` 的 release 与 tag。

### 2026-11-06（v0.1.0-rc.2：--data-root）

- `install.sh` 新增 `--data-root <dir>`，默认 `/srv/dictation`，支持
  `/var/lib/dictation`。
- 新生成的 `config.toml` 的 `paths.base`、managed unit 的 `ReadWritePaths`
  自动使用该目录；已有 config/unit/data 保留。
- 本地 mock release 实测：
  `--root ... --data-root /var/lib/dictation --no-systemd` 安装成功，
  config base 与 unit ReadWritePaths 均为 `/var/lib/dictation`，二次安装配置不变。
- 版本 `0.1.0-rc.2`；待 commit + tag 发布。

### 2026-11-06（v0.1.0-rc.1 发布准备）

- 文档设备泛化：只写最低配置（2 核 / 1GB RAM / 8GB 磁盘 / glibc ≥2.30）。
- 安全扫描：`.env`、`data/`、`lessons/`、`cache/`、`demo/`、`target/`、`dist/`
  均 gitignore；未发现真实 key。
- License：MIT；新增 `LICENSE`，README 增加 License 段。
- Git 初始化；public 仓库：`https://github.com/babydragon/dictation`。
- 新增 `.github/workflows/release.yml`：
  test → x86_64/riscv64 构建 → tgz（含 web/deploy/docs/skills/LICENSE/install.sh）
  → SHA256SUMS → release/pre-release。
- 新增 `install.sh`：
  release/镜像下载、SHA256 校验、路径穿越检查、幂等安装、保留 config/env/data、
  managed unit 更新、systemd、健康检查；支持 `--dry-run`/`--root`/`--no-systemd`。
- 本地 mock release 实测：安装、二次幂等、dry-run、checksum 失败均符合预期。
- 首次 commit/push 与 tag：已完成（commit `84bf7e3`，tag `v0.1.0-rc.1`）。

### 2026-10-22（SBC 交叉编译验证）

- 本机 `cargo zigbuild --release --target riscv64gc-unknown-linux-gnu` 通过（35.5s）。
- 产物 ELF 64-bit RISC-V，PIE，glibc，最大 GLIBC_2.30，7.0MB。
- `libsqlite3-sys`（bundled C SQLite）与 `ring`（C/asm）均由 zig cc 编译成功。
- 仅有 linker warning：`ignoring deprecated linker optimization setting '1'`。
- 未做板上运行（本机无 qemu-riscv64）。

---

## 8. 依赖 / 交叉编译审计

- 目标：Linux SBC `riscv64gc-unknown-linux-gnu`，glibc ≥2.30，内存 ≥2GB。
- 构建：优先本地 `cargo zigbuild`。
- 约束：新增依赖优先纯 Rust；**尽可能避免 `*-sys`**（`openssl-sys`、
  `libsqlite3-sys` 等），降低交叉编译风险。
- 当前已知：
  - `rusqlite(bundled)` → `libsqlite3-sys`（C SQLite）。SBC 前必须验证 zigbuild；
    失败则评估 `redb` / JSON 等纯 Rust 存储。
  - `reqwest` 使用 rustls，避免 OpenSSL；`ring` 的 C/asm 在 riscv64 上需验证。
  - `ffmpeg` 是外部命令，不是 crate 依赖。
- 审计清单：
  - [x] `cargo tree` 找出所有 `*-sys`：当前只有 `libsqlite3-sys v0.30.1`
        （来自 `rusqlite` bundled）；另有 `ring v0.17.14`（C/asm，不是 -sys）。
  - [x] riscv64 / zigbuild 编译：`cargo zigbuild --release --target
        riscv64gc-unknown-linux-gnu` 通过（35.5s）。
        - 产物：`target/riscv64gc-unknown-linux-gnu/release/dictation`
        - ELF 64-bit RISC-V，PIE，动态链接 glibc；最大 GLIBC_2.30；7.0MB。
        - `libsqlite3-sys` bundled C SQLite 由 zig cc 编译成功。
        - `ring` 的 C/asm 在 riscv64 编译成功。
        - 仅有 linker warning：`ignoring deprecated linker optimization setting '1'`。
  - [x] 在 SBC 板上实际运行：安装、systemd 开机自启、手机访问、
        服务端生成（HTTP 导入 → 生成）均已实测。
  - [ ] 记录无法替代的例外与理由（`libsqlite3-sys` 目前保留）。

## 9. 代码层已知小缺口

- [ ] `cue` 只解析 `beep|none`，`render` 还没生成提示音；要么实现短 beep，要么从输入格式移除。
- [ ] `review_cap` 只在 `lesson.toml` 解析，服务端自动混入固定 5 个；需要把 cap 带进 session。
- [x] `dictation clean --unused` 已实现（默认 dry-run，`--apply` 才删）。
- [ ] Qwen 真实网络 smoke：暂不使用，代码保留但未验证（当前用火山豆包）。
- [x] 断点续听已实现（localStorage，7 天；完成/提交清除）。
- [ ] PWA 没有 Service Worker；iOS 局域网 HTTP 不支持离线缓存（SBC 常开即可）。
- [ ] 多音字仍靠 `speak=` 手工覆盖；没有自动拼音/多音字词典。
- [ ] 答案纸/错词本尚未实现；检查页只在屏幕上显示。

## 10. 维护规则

1. 完成一个功能块：更新本文件「当前状态」、勾选验收项、追加工作日志。
2. 只有设计/约定变化时才改 `AGENTS.md`；不要为了进度改它。
3. 每次改动跑 `cargo fmt --check`、`cargo clippy --all-targets -- -D warnings`、
   `cargo test`；前端改动加 `node --check web/app.js`。
4. 不在本文件写密钥、孩子照片、真实姓名。
