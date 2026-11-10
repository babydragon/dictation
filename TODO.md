# TODO / 进度与验收

> 本文件是**进度、验收、工作日志的唯一来源**。
> `AGENTS.md` 只保留稳定设计与约定；每次工作完成后更新本文件，不要为了进度去改
> `AGENTS.md`（除非设计本身变了）。
>
> 最后更新：2026-11-09

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
| 复习加练 | ✅ `limit`(1–40)、`grade`/`term` 筛选、科目卡片显示逾期 |
| AI 组卷 | ✅ `/api/review/queue`、`/api/sessions/custom`、`GET /api/sessions/{id}`、`?s=` 深链 |
| 缓存清理 | ✅ `clean --unused/--older-than`，默认 dry-run |
| 断点续听 | ✅ localStorage 保存 session + item，7 天有效 |

当前测试：`cargo test` **188 passed**（179 lib + 9 bin）；clippy / fmt 干净；
release 构建通过。
演示课时：`lessons/第5课`（20 词，3:24，运行时数据，不提交）。

## 2. 下一步（按优先级）

> SBC 部署已验收（安装 + systemd 自启 + 手机访问 + 服务端生成），
> 不再是待办。离线本地 TTS 仍为可选增强。

1. **家长模式 / 答案纸**：运行中调 gap/rate/repeat；A4 一列 20 题、带题号。
2. **拍照批改**：`Grader` trait + 多模态模型 + 置信度 + 人工确认。
3. **记忆曲线细化**：`review_cap` 接到服务端、FSRS、按教材/单元限定范围。
4. **本地 TTS（可选）**：断网兜底，SBC 上选型与实测。
5. **F8 外网分享**：Cloudflare Pages + R2，独立会话实施；要点见 §5 F8。

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

### F8 外网分享（Cloudflare Pages + R2，计划中，未实现）

目标：课时发布到 CF，非家庭网络也能给孩子听写；结果导出后回家导入，形成复习闭环。
详细契约见本地工作稿 `docs/share-publish.md`（未入库，独立会话实施时先读它）。

**架构与边界**

- 双通道：Pages 只放壳（`web/` + 生成的 `config.js` + `_headers`）；R2 只放数据。
  发布新课时不重新部署 Pages；R2 是唯一真相源，本地不留发布账本。
- 只读发布，不做 R2 写接口、不做多租户；v1 不加访问控制（以后一条 WAF 规则即可）。
- 静态站不包含复习词（`review` 恒 false），复习仍靠本地 SQLite。

**发布物**（`<dataBase>/`）

- `lessons/index.json`：`{schema_version,updated_at,lessons[{id,title,item_count,total_ms,path,content_hash,published_at}]}`，派生缓存，可重建。
- `lessons/<id>/lesson.json`：与 `SessionManifest` 同形；新增 `schema_version`/`lesson_id`/`content_hash`，省略 `session_id` 与 `base`；`full_track` 仅 `--with-full` 时出现。
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

**静态前端**

- 注入 `config.js`：`DICTATION_MODE='static'`、`DICTATION_DATA_BASE=...`（不用 query 参数，避免刷新/加主屏丢失）。
- 只替换 5 个调用：`lessons()`/`createSession()`/`reviewSummary()`（恒 0）/`createReview()`（不支持）/`check()`（本地判分）。
- 深链 `?l=<lesson_id>` 直接进播放页；失效回落列表并提示“这一课可能已下线”。
- 保留时间轴、暂停/重听、断点续听、检查页、结果页。

**结果回传**

- 导出 `dictation.result/v1`（自包含 items + `content_hash` + `finished_at`）；移动端三级降级：`navigator.share` → `Blob` 下载 → 文本框复制。
- `POST /api/results/import`：幂等键 = 规范化 `{content_hash,finished_at,student,items[position/text/correct]}` 的 sha256，忽略客户端 id；
  已导入 → `duplicate`；时间基准用 `finished_at`（未来 >5 分钟钳制为 now）；单事务写 `sessions(kind=imported)` + `session_items` + `attempts(source=import)` + `review_state`。
- CLI 等价：`dictation result import <file|-> --json`。

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
- 下一步：设备上验证（科目/复习组卷/加练/AI 组卷），再决定 `v0.2.0` 正式版。



- `GET /api/review/queue`：返回可播放的待复习词（错词→最久未复习排序），
  支持 `limit`(1–200)、`grade`/`term`；`total` 不受 limit 截断。
- `POST /api/sessions/custom`：按 agent 给定的词表与顺序创建临时复习会话；
  1–60 个词，重复/未知/缺音频分别 400；不建课时目录、不调 TTS。
- `GET /api/sessions/{id}`：重放已创建 session；课时 session 重新读 manifest，
  复习/自定义 session 用复习默认值。
- schema v3：`session_items` 增加 `repeat_gap_ms`，`GET /api/sessions/{id}` 可精确重放
  （否则非默认 repeat_gap 的课时会按 1200ms 播放）。
- PWA 支持 `?s=<session_id>` 深链：首页出现「点击开始」卡片（规避 iOS 自动播放限制）。
- skill 新增「复习组卷 / 加练」流程，写明 60 词上限、错误处理、agent 只选词不排期。
- 验证：188 tests；fmt / clippy / JS 通过。

### 2026-11-09（复习加练）

- `POST /api/sessions/review` 支持 `limit`（默认 20，范围 1–40）、`grade`（1–6）、
  `term`（1–2）；非法值 400。
- `create_review_session_filtered`：按 `words.clip_lesson → lessons.grade/term`
  过滤来源课；选词排序沿用「错词 → 最久未复习 → due_at」；session 记录
  subject/grade/term。
- 首页科目复习卡显示「最久 N 天未复习」，便于判断是否需要加练。
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
