---
name: dictation-prep
description: 把老师消息、课本照片、杂乱词表整理成 dictation 项目可用的课时，并通过 CLI 交给主机写入；也可查询听写结果、提取错词做记忆分析、清理 TTS 缓存。用于“准备听写”“整理词表”“生成听写音频”“分析错词”“看听写结果”“清理缓存”等场景。
---

# 听写词表准备（CLI 导入版）

你的任务不是教孩子，而是把家长手里的**原始材料**变成 `dictation` 能吃的
**纯文本词表 + 课时设置**，然后通过 CLI 交给主机写入。

> **不要用文件工具直接读写 `/srv`、`/opt` 或主机上的任何路径。**
> agent 可能在沙箱里，没有主机目录权限。所有写入都通过 `dictation lesson import`
> 完成；CLI 会按 `/etc/dictation/config.toml`（或 `DICTATION_CONFIG`）解析路径。

## 输入

- 老师发的微信文字，带“1. 2. 3.”、“读两遍”、“听写第5课”等。
- 课本/作业本照片（你有视觉能力时直接读）。
- 家长随口列的一串词。
- 已经整理好的列表，但格式混乱。

## 输出方式

CLI 会把内容写到配置里的 `lessons` 目录，并返回实际路径。你不需要知道
`/srv/dictation` 是否存在，也不需要 `mkdir`。

```bash
dictation lesson import --payload - --json
```

stdin 是一个 JSON：

```json
{
  "title": "第5课 词语听写",
  "dir": "第5课",                     // 可选；不给则从 title 生成安全目录名
  "text": "摇晃\n粗壮\n毕竟\n",
  "toml": "rate = 0.9\ngap = \"6s\"\nrepeat = 2\n",
  "force": false
}
```

- `text`：**一行一个词**，保持老师给的顺序；不要加标题行（CLI 会写 `# title`）。
- `toml`：可选；不写则用默认设置。字符串里的换行用 `\n`。
- `force`：默认 `false`；目标课时已存在时 import 会报错。确认要覆盖后再设 `true`。
- 成功返回：

```json
{
  "created": true,
  "dir": "/srv/dictation/lessons/第5课",
  "title": "第5课 词语听写",
  "item_count": 20,
  "total_ms": 181160
}
```

如果 `dictation` 不在 PATH，用 `/opt/dictation/dictation`。
配置不在默认位置时，先让 CLI 通过环境变量找到它：

```bash
export DICTATION_CONFIG=/path/to/config.toml
```

## 硬规则

- **不造词、不改词、不扩写。** 只做格式归一化。
- **保持顺序。** 老师给的顺序通常就是听写顺序。
- 去掉序号、项目符号、前后引号、多余空白；中文标点保留在句子里。
- 空行忽略；重复词只保留第一次，并在回复里指出“重复：xxx”。
- 英文单词/短语保持原样；不要把英文翻译成中文。
- 不要自动加拼音；只有家长明确要求时才生成答案纸。
- 不要自动加 `rate=` / `gap=`；只有明确需要放慢/加长时才加。
- **不要绕过 CLI 写文件**；不要尝试读取主机路径来“确认一下”。
- **先 import，后 gen。** import 只写词表/设置，不调 TTS；`gen` 才花钱。

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
review_cap = 5
```

- 默认不逐题播报“第 N 个”。
- 开场白/结束语由程序按内容语言自动选择（全英文 → 英文，否则中文）；
  不要自己往 `text` 里加题号或结束语。
- **不要把 `DICTATION_PROVIDER` 写进 `lesson.toml`**；provider 由 CLI 环境/参数决定。

需要调整的典型情况：

- 词很长/句子：`gap = "10s"`，或给单条加 `gap=12s`。
- 孩子觉得太快：`rate = 0.8`。
- 只读一遍：`repeat = 1`。
- 英文为主：在 `lesson.toml` 里设 `voice_en`，不要把中文 voice 用于英文。
- 想改开场白/结束语：在 `toml` 里设 `intro` / `outro`；空串关闭。

## 工作流

1. 读取原始材料，按规则生成 `text` 和可选的 `toml` 字符串。
2. 可选：先查重/确认已有课时：
   ```bash
   dictation lesson list --json
   ```
3. 把 JSON 通过 stdin 交给 CLI：
   ```bash
   dictation lesson import --payload - --json
   ```
   例：
   ```bash
   printf '%s' '{"title":"第5课","text":"摇晃\n粗壮\n","toml":"rate = 0.9\ngap = \"6s\"\n"}' \
     | dictation lesson import --payload - --json
   ```
4. 把 import 返回的信息用 3–5 行回报给家长：
   - 实际目录 `dir`
   - 词数 `item_count`
   - 预计总时长 `total_ms`
   - 是否有重复/超长/疑似多音字
   - 本次 `rate` / `gap` / `repeat`
5. **等待家长确认。**
6. 确认后生成音频：
   ```bash
   dictation lesson gen --title "第5课" --provider doubao --full-track
   ```
   - 如果 `title` 带空格/特殊字符，用 `--dir` 指定 import 返回的目录名。
   - 未配置 API key 时用 `--provider mock` 验证流程，并提醒家长这不是最终音频。
   - 生成失败（缺 key/网络）时报告错误，不要在聊天里索要 key。
7. 回报产物：`manifest.json` 路径、`full.mp3`、片段数量、TTS 调用/缓存命中。

如果 import 报“课时已存在”：

- 默认不要覆盖；问家长是否要更新。
- 确认更新后，重新 import 并在 payload 里设 `"force": true`。
- 覆盖后需要重新 `lesson gen`，否则音频还是旧的。

## 更新已有课时

```bash
dictation lesson list --json          # 找 title/dir
dictation lesson import --payload - --json   # force=true
dictation lesson gen --title "第5课" --provider doubao --full-track
```

不要用文件工具直接改 `lesson.txt`；所有修改都走 import。

## 在 SBC / 沙箱里运行

- 只要 agent 能执行 `dictation`，CLI 会以当前用户权限写入配置里的
  `/srv/dictation/lessons`；agent 不需要主机目录读取权限。
- 你的用户需要在 `dictation` 组（安装时设置）；加组后重新登录。
- TTS key 由 CLI 进程的环境提供（例如 `~/.config/dictation/env`）；
  agent 不要读取或打印 key。
- 如果沙箱**完全不能执行主机 CLI**，停止，并向家长说明需要在主机侧执行；
  不要尝试猜路径或用文件工具绕过。

## 查询听写结果 / 分析错词

家长说“看下这次听写”“分析一下错词”“为什么这些词老错”时：

```bash
# 最近已提交检查的听写
dictation result list --json

# 某次完整结果
dictation result show --latest --json
# 或
dictation result show <session_id> --json

# 只取错词（推荐给大模型）
dictation result wrong --latest --json
```

`wrong_words` 里每个错词包含：

- `text` / `lang`
- `origin_lesson`（来自哪一课；自动混入的复习词会指回原课）
- `position` / `review`
- `lapses` / `interval_days` / `due_at`
- `grade` / `source`

分析规则：

- **只使用 CLI 返回的 `wrong_words`**，不要自己补词或猜错因。
- 可以按类型归纳：同音字、形近字、笔顺、前后鼻音、英文拼写/发音。
- 给出具体记忆钩子：拆字、组词、对比、造句、口诀；不要泛泛说“多读多写”。
- 可以建议下次复习顺序/分组，但**不要**直接修改课时或调用 gen；
  需要新增复习课时时，先按前面流程生成并让家长确认。
- 如果 `result list` 为空或 `wrong` 报“还没提交检查”，直接说明，不要编造结果。

## 清理 TTS 缓存

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
