# dictation

把一份纯文本词表变成节奏精确的听写音频/播放会话。

> 稳定设计与约定见 [`AGENTS.md`](AGENTS.md)；进度、验收、工作日志见
> [`TODO.md`](TODO.md)；安装/交叉编译/systemd 见 [`INSTALL.md`](INSTALL.md)。
> 本文件只放快速上手。

## 安装（发布包）

当前正式版 `v0.1.0`；预发布 `v0.2.0-rc.1`（科目隔离、复习组卷、AI 组卷）。
预发布使用 `-rc.x` 后缀，例如 `v0.2.0-rc.1`。

```bash
# 指定版本（推荐，可复现）
curl -fsSL https://github.com/babydragon/dictation/releases/download/v0.1.0/install.sh \
  | sudo bash -s -- --version v0.1.0

# 最新正式版（自动解析 latest，不含 pre-release）
curl -fsSL https://github.com/babydragon/dictation/releases/latest/download/install.sh \
  | sudo bash
```

安装脚本会：

- 下载对应架构的 tgz（x86_64 / riscv64）；
- 校验 `SHA256SUMS`；
- 安装到 `/opt/dictation`、`/srv/dictation`、`/etc/dictation`；
- 保留已有 `config.toml`、env、课时和数据库；
- 安装/更新 systemd unit 并重启服务。

数据目录默认 `/srv/dictation`；如果更喜欢 FHS 的服务状态目录，可以：

```bash
... | sudo bash -s -- --version v0.1.0 --data-root /var/lib/dictation
```

`--dry-run` 只下载/校验并打印计划，不写系统；`--no-systemd` 跳过服务安装。
详见 [`INSTALL.md`](INSTALL.md)。

## 快速开始

```bash
# 1. 准备词表：一行一个词
cat > fixtures/lesson-20.txt <<'EOF'
# 第5课
摇晃
粗壮
毕竟
EOF

# 2. 只看时长，不调用 TTS、不花钱
cargo run -- plan fixtures/lesson-20.txt

# 3. 合成（无密钥时先用 mock 验证流程）
cargo run -- gen fixtures/lesson-20.txt --provider mock --out lessons/第5课 --full-track

# 4. 用真实 TTS（豆包 V3）
export DOUBAO_API_KEY=...
cargo run -- gen fixtures/lesson-20.txt --provider doubao --out lessons/第5课 --full-track

# 或者百炼 Qwen
# export DASHSCOPE_API_KEY=sk-...
# cargo run -- gen fixtures/lesson-20.txt --provider qwen --out lessons/第5课 --full-track
```

产物：

```
lessons/第5课/
├── manifest.json
├── clips/01-ab12cd.wav
├── full.wav          # 始终生成
└── full.mp3          # --full-track 时导出
```

`manifest.json` 是播放器的唯一入口；`full.mp3` 可直接拷到任何播放器。
`gap` 是插入的静音；由于片段保留 20ms/60ms 留白，可听静音约为 `gap + 80ms`。

## 词表格式

最小形式就是一行一词，编号/项目符号/空行都能容错：

```text
1. 摇晃
2. 粗壮
3. apple
```

可选 `lesson.toml`（同目录，由 agent 生成，家长不用手写）：

```toml
title = "第5课"
rate = 0.9
gap = "6s"
repeat = 2
# intro = "请准备好纸和笔，听写马上开始。"  # 可选；空串关闭
# intro_gap = "1.5s"
# outro = "听写结束，请检查一遍。"          # 可选；空串关闭
# announce_index = false                  # 默认不逐题报“第 N 个”
```

默认会按内容语言自动加开场白和结束语（全英文用英文），相同文本/音色/语速只生成一次。
逐题题号默认关闭；结束语在最后一个词的等待之后播放。

单词级覆盖：

```text
摇晃 | rate=0.8
旅行 | gap=8s
长大 | speak=长大的长
```

## TTS 配置

```bash
# 百炼 Qwen-TTS
export DASHSCOPE_API_KEY=sk-...
export QWEN_TTS_MODEL=qwen3-tts-flash
export QWEN_VOICE_ZH=Cherry
export QWEN_VOICE_EN=Aiden

# 豆包 TTS V3（seed-tts-2.0）
export DOUBAO_API_KEY=...
export DOUBAO_RESOURCE_ID=seed-tts-2.0
export DOUBAO_VOICE_ZH=zh_female_vv_uranus_bigtts
export DOUBAO_VOICE_EN=en_female_stokie_uranus_bigtts
```

豆包走 V3 `seed-tts-2.0`，只需要 `DOUBAO_API_KEY`；`DOUBAO_MODEL` 仅复刻音色时需要。
默认音色为 Vivi 2.0（中）和 Stokie（英，美式女声）。

> Qwen provider 代码保留，但**尚未做真实网络 smoke 验证**；当前日常使用火山豆包
> （有赠送额度）。等后续有 `DASHSCOPE_API_KEY` 再验证。

密钥只放环境变量，不写入仓库。

## 孩子端播放器

```bash
cargo run -- serve
# 或读 /etc/dictation/config.toml（SBC 正式部署）
# cargo run -- --config /etc/dictation/config.toml serve
# 或指定端口/数据库：
# cargo run -- serve --dir lessons --web web --data data/dictation.db --host 0.0.0.0 --port 8080
```

手机/平板与电脑在同一 WiFi，打开终端打印的 `http://<电脑IP>:8080`。

流程：首页选课时 → 播放（暂停/重听当前词）→ 完成 → 检查（只标错）→
提交 → 首页出现「错词复习」。错词复习标对后入口消失。

中途返回首页会保留进度；7 天内首页出现「继续上次听写」，从上次的词继续。
完成或提交检查后进度清除。

下一课会自动带上最多 5 个未纠正/到期的错词，追加在本课词之后、结束语之前，
不加语音提示；检查页会用「复习」小标签区分。

演示课时：`lessons/第5课`（20 词，约 3:24）。这是运行时数据，不会提交到仓库。

> 服务默认监听 `0.0.0.0`，仅限家庭局域网使用；没有账号体系，不要暴露到公网。
> 需要时可用 `DICTATION_API_TOKEN` 保护导入/生成接口。

## HTTP API（agent 用）

`serve` 同时是 agent 的唯一入口：导入词表、生成音频、查进度都走 HTTP，
agent 不需要文件系统权限，也不需要 `dictation` 在 PATH 里。

```bash
API=http://127.0.0.1:8080

curl -sS "$API/api/health"

# 导入（201 新建 / 200 覆盖 / 409 已存在）
curl -sS -X POST "$API/api/lessons" -H 'content-type: application/json' -d '{
  "title": "第5课",
  "subject": "chinese",
  "text": "摇晃\n粗壮\n毕竟\n",
  "toml": "rate = 0.9\ngap = \"6s\"\n"
}'

# 生成（202 返回 job_id；已是最新则 skipped）
curl -sS -X POST "$API/api/lessons/第5课/generate" -H 'content-type: application/json' -d '{"full_track": true}'

# 轮询
curl -sS "$API/api/jobs/<job_id>"

# 结果查询（只读，GET 不需要 token）
curl -sS "$API/api/results?limit=10"        # 最近已提交检查的听写
curl -sS "$API/api/results/latest"          # 最近一次完整结果
curl -sS "$API/api/results/latest/wrong"    # 只取错词
```

设置 `DICTATION_API_TOKEN` 后，写操作需带 `X-Dictation-Token`。
完整契约见 [`docs/http-api.md`](docs/http-api.md)。

## 命令

| 命令 | 作用 |
|---|---|
| `dictation plan <lesson>` | 只算每条/总时长，不调 TTS |
| `dictation gen <lesson>` | 合成片段 + manifest + 整轨 |
| `dictation new "第5课"` | 生成词表模板 |
| `dictation serve` | 启动局域网播放器 + 自检/错词复习 |
| `dictation config show` | 显示解析后的路径/server 配置 |
| `dictation lesson import --payload -` | 主机侧导入（运维/调试；agent 改用 HTTP） |
| `dictation lesson plan/gen/list` | 按 title 预览/生成/列课时 |
| `dictation result list/show/wrong` | 查最近听写、逐项结果、错词 JSON |
| `dictation clean --unused [--apply]` | 清未引用缓存；默认 dry-run |

`/etc/dictation/config.toml` 管理 `paths`/`server`。**agent 通过 HTTP API 写入**
（`POST /api/lessons`、`POST /api/lessons/{id}/generate`），不 exec CLI、
不读写主机目录；CLI 保留为运维入口。详见 [`INSTALL.md`](INSTALL.md) 和
[`skills/dictation-prep/SKILL.md`](skills/dictation-prep/SKILL.md)。

## License

MIT。依赖均为宽松许可（MIT / Apache-2.0 / ISC / BSD / Unicode-3.0 / Zlib 等）；
详见 `Cargo.lock` 与各 crate 的 license 字段。

## 当前状态

F1-F4 v1 已可用：词表 → 豆包 V3 音频、儿童端播放/自检、SQLite 记录、
错词复习与下一课自动混入。手机/平板实机验证通过。

详细状态、验收项和工作日志见 [`TODO.md`](TODO.md)；
家长模式、拍照批改、SBC 部署是后续功能块。

试听样本（豆包 V3，含开场白 + 5 词 + 结束语，无逐题题号）：

- `demo/smoke-zh.mp3`：中文 5 词，57.96s，Vivi 2.0。
- `demo/smoke-en.mp3`：英文 5 词，59.29s，Stokie。

中英文响度已按 RMS 均衡到 -19 dBFS。
