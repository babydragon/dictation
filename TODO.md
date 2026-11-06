# TODO / 进度与验收

> 本文件是**进度、验收、工作日志的唯一来源**。
> `AGENTS.md` 只保留稳定设计与约定；每次工作完成后更新本文件，不要为了进度去改
> `AGENTS.md`（除非设计本身变了）。
>
> 最后更新：2026-10-22

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
| 缓存清理 | ✅ `clean --unused/--older-than`，默认 dry-run |
| 断点续听 | ✅ localStorage 保存 session + item，7 天有效 |

当前测试：`cargo test` **127 passed**（118 lib + 9 bin）；clippy / fmt 干净；
release 构建通过。
演示课时：`lessons/第5课`（20 词，3:24，运行时数据，不提交）。

## 2. 下一步（按优先级）

1. **SBC 部署**：交叉编译已通过；下一步把二进制 + `web/` 拷到 SBC，
   实际启动、浏览器访问、systemd 常驻；再做本地 TTS 选型。
2. **家长模式 / 答案纸**：运行中调 gap/rate/repeat；A4 一列 20 题、带题号。
3. **拍照批改**：`Grader` trait + 多模态模型 + 置信度 + 人工确认。
4. **记忆曲线细化**：`review_cap` 接到服务端、FSRS、按教材/单元限定范围。

### 发布 v0.1.0-rc.1（进行中）

- [x] 文档设备泛化 + 最低配置要求
- [x] 密钥/运行时文件扫描（`.env`、`data/`、`lessons/`、`cache/`、`demo/` 均 gitignore）
- [x] Git 仓库初始化：`https://github.com/babydragon/dictation`（public）
- [x] License：MIT（依赖均为宽松许可）
- [x] GitHub Actions：测试 → x86_64/riscv64 构建 → tgz → release/pre-release
- [x] `install.sh`：下载/校验/幂等安装/保留配置
- [x] 本地 mock release 安装、二次幂等、dry-run、checksum 失败验证
- [x] 首次 commit + push + tag `v0.1.0-rc.1`
- [x] GitHub Actions 全流程通过；pre-release 已发布
- [x] 从 GitHub release 实测 `install.sh --root` 安装；x86 二进制 rc.1、RISC-V 资产校验通过
- [ ] 在目标 SBC 上真实安装 + systemd 启动（待上板）

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

## 5. F5-F7 计划

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

---

## 6. 验证记录

### 6.1 自动化

- `cargo test`：127 passed（截至 2026-10-22）。
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

### 2026-10-20（P1 v1）

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
  - [ ] 在 SBC 板上实际运行（qemu 未安装，未做用户态执行）。
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
