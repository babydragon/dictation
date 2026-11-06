# 安装与部署

> 适用：本机开发运行，以及 Linux 主机/SBC（x86_64 或 riscv64，glibc ≥2.30）常驻。
> 稳定设计见 [`AGENTS.md`](AGENTS.md)；进度与验收见 [`TODO.md`](TODO.md)。

---

## 0. 一键安装（Release / Pre-release）

测试版使用 `-rc.x`：

```bash
curl -fsSL https://github.com/babydragon/dictation/releases/download/v0.1.0-rc.1/install.sh \
  | sudo bash -s -- --version v0.1.0-rc.1

# 最新正式版
curl -fsSL https://github.com/babydragon/dictation/releases/latest/download/install.sh \
  | sudo bash
```

`install.sh` 会：

- 按 `uname -m` 选择 x86_64 / riscv64 资产；
- 校验 `SHA256SUMS`；
- 安装到 `/opt/dictation`、`/srv/dictation`、`/etc/dictation`；
- 创建/复用 `dictation` 用户与组；
- **保留已存在的 `config.toml`、env、课时、数据库、缓存**；
- 安装/更新 systemd unit（带 managed 标记的才更新）并重启服务；
- 安装后检查 `/api/health`。

常用参数：

```bash
install.sh --version v0.1.0-rc.1   # 指定 rc
install.sh --dry-run               # 只下载/校验/打印计划
install.sh --no-systemd            # 只安装文件，不装服务
install.sh --repo owner/repo       # 覆盖默认仓库
```

最低要求（服务端）：

- Linux x86_64 / riscv64，glibc ≥2.30；
- 2 核 CPU、1GB RAM（建议 2GB）；
- 8GB 可用磁盘（建议 20GB+；SSD/NVMe 更佳）；
- 有线或无线局域网；systemd（使用一键安装时）；
- 本地 TTS 可选，建议 4GB+ RAM 和额外 2GB 磁盘。

## 0.1 从源码安装

两种方式：

| 方式 | 适用 | 产物 |
|---|---|---|
| A. 本机运行 | 先在电脑上试用、生成课时 | `target/release/dictation` + `web/` |
| B. SBC 常驻 | 家里手机/平板随时访问 | RISC-V 二进制 + `web/` + `lessons/` + systemd |

推荐分工：**电脑负责 `dictation gen` 生成课时，SBC 只跑 `dictation serve`**。
这样 SBC 不需要 TTS key，也不需要 ffmpeg。

---

## 1. 依赖

### 本机

- Rust 1.85+（实测 1.98.1）
- C 编译器（`gcc` / `cc`）：`rusqlite` 的 bundled SQLite 需要编译 C
- `ffmpeg`：导出 `full.mp3`、Qwen 语速后处理需要；只播放 WAV 时可不装
- Node.js 仅用于 `node --check web/app.js`，运行不需要

```bash
# Debian/Ubuntu 示例
sudo apt update
sudo apt install -y build-essential ffmpeg
```

### 交叉编译（在 x86_64 电脑上）

```bash
rustup target add riscv64gc-unknown-linux-gnu
cargo install cargo-zigbuild
zig version        # 实测 0.16.0
```

- SBC 运行需要 **glibc ≥ 2.30**（当前二进制最高引用 `GLIBC_2.30`）。
- SBC 不需要系统 SQLite、OpenSSL；`rusqlite` 已 bundled，`reqwest` 用 rustls。
- 交叉编译本身不需要 ffmpeg；只有要在 SBC 上 `gen` 才需要。

### API key

豆包 V3：

```bash
DOUBAO_API_KEY=...
DOUBAO_RESOURCE_ID=seed-tts-2.0
DOUBAO_VOICE_ZH=zh_female_vv_uranus_bigtts
DOUBAO_VOICE_EN=en_female_stokie_uranus_bigtts
DICTATION_PROVIDER=doubao
```

百炼 Qwen（可选）：

```bash
DASHSCOPE_API_KEY=sk-...
QWEN_TTS_MODEL=qwen3-tts-flash
QWEN_VOICE_ZH=Cherry
QWEN_VOICE_EN=Aiden
```

`dictation` **不会自动读取 `.env`**；shell 里用 `source`，systemd 用
`EnvironmentFile`。

---

## 2. 方式 A：本机运行

### 2.1 构建

```bash
cd /path/to/dictation
cargo build --release
./target/release/dictation --version
```

可选验证：

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

### 2.2 安装二进制（可选）

```bash
sudo install -Dm755 target/release/dictation /usr/local/bin/dictation
```

或只放到用户目录：

```bash
mkdir -p ~/.local/bin
install -m755 target/release/dictation ~/.local/bin/dictation
# 确保 ~/.local/bin 在 PATH
```

### 2.3 准备词表与 key

```bash
cp .env.example .env
chmod 600 .env
# 编辑 .env：至少填 DOUBAO_API_KEY，并设置 DICTATION_PROVIDER=doubao

set -a
source .env
set +a
```

### 2.4 生成课时

```bash
dictation plan lesson.txt                 # 只算时长，不调 TTS
dictation gen lesson.txt \
  --provider doubao \
  --out lessons/第5课 \
  --full-track
```

产物：

```
lessons/第5课/
├── manifest.json
├── clips/*.wav
├── full.wav
└── full.mp3
```

### 2.5 启动服务

```bash
# 在项目根目录（默认 --dir lessons --web web --data data/dictation.db）
dictation serve

# 或显式指定
dictation serve \
  --dir lessons \
  --web web \
  --data data/dictation.db \
  --host 0.0.0.0 \
  --port 8080
```

手机/平板与电脑同一 WiFi，打开终端打印的地址：

```
http://<电脑IP>:8080
```

---

## 3. 方式 B：SBC 交叉编译

### 3.1 构建

在 x86_64 开发机、项目根目录：

```bash
cargo zigbuild --release --target riscv64gc-unknown-linux-gnu
```

产物：

```
target/riscv64gc-unknown-linux-gnu/release/dictation
```

实测：

- ELF64 RISC-V，PIE
- 7.0 MB（stripped）
- 最大 `GLIBC_2.30`
- `libsqlite3-sys`（bundled C SQLite）与 `ring`（C/asm）均由 zig cc 编译成功
- 唯一 warning：`ignoring deprecated linker optimization setting '1'`，无害

### 3.2 如果 SBC 的 glibc 更老

在 SBC 上：

```bash
ldd --version | head -1
```

若低于 2.30，用带 glibc 版本后缀的 target 重新编：

```bash
cargo zigbuild --release --target riscv64gc-unknown-linux-gnu.2.28
```

（2.27/2.28 按 SBC 实际版本选。）

### 3.3 SBC 目录、用户与权限

推荐把**只读程序**和**可写数据**分开：

| 路径 | 用途 | 属主/权限 |
|---|---|---|
| `/opt/dictation` | 二进制、`web/`、INSTALL | `root:root`，只读 |
| `/srv/dictation/lessons` | 生成的课时 | `dictation:dictation`，2775（setgid） |
| `/srv/dictation/data` | SQLite | `dictation:dictation`，0750（服务专用） |
| `/srv/dictation/cache` | TTS 片段缓存（可选） | `dictation:dictation`，2775 |

```bash
# 用户已存在可跳过 useradd
sudo useradd --system --home /srv/dictation --shell /usr/sbin/nologin dictation

# 让生成课时的用户（你/agent）加入 dictation 组；加完要重新登录
sudo usermod -aG dictation "$USER"

sudo mkdir -p /opt/dictation/web /srv/dictation/{lessons,data,cache}
sudo chown root:root /opt/dictation
sudo chmod 755 /opt/dictation

sudo chown -R dictation:dictation /srv/dictation
sudo chmod 2775 /srv/dictation/lessons /srv/dictation/cache   # 新文件继承 dictation 组
sudo chmod 0750 /srv/dictation/data                           # 只有服务用户可写
```

验证：

```bash
id                                  # 应看到 dictation 组
namei -l /srv/dictation/lessons     # 逐级检查权限
```

> 为什么这样做：`/opt` 里的程序不需要被 agent 修改；agent 只需要写
> `/srv/dictation/lessons` 和 `cache`。`data/` 里的 SQLite 由服务独占写入，
> agent 不需要也不应该直接改。

### 3.4 复制二进制与前端

在开发机：

```bash
scp target/riscv64gc-unknown-linux-gnu/release/dictation k1:/tmp/dictation
scp -r web k1:/tmp/dictation-web
scp deploy/dictation.service k1:/tmp/dictation.service
scp deploy/config.example k1:/tmp/dictation-config.example
scp deploy/env.example k1:/tmp/dictation-env.example
scp INSTALL.md k1:/tmp/dictation-INSTALL.md
```

在 SBC：

```bash
sudo install -Dm755 /tmp/dictation /opt/dictation/dictation
sudo cp -r /tmp/dictation-web/* /opt/dictation/web/
sudo install -Dm644 /tmp/dictation-INSTALL.md /opt/dictation/INSTALL.md

# 程序目录保持 root 只读
sudo chown -R root:root /opt/dictation
sudo chmod -R go-w /opt/dictation
```

> `web/` 是必须的；二进制不内嵌前端。缺 `web/index.html` 时 `/` 会 500。

### 3.5 复制课时

推荐在开发机生成，再同步：

```bash
rsync -av lessons/ k1:/tmp/dictation-lessons/
ssh k1 'sudo mkdir -p /srv/dictation/lessons && \
  sudo cp -a /tmp/dictation-lessons/. /srv/dictation/lessons/ && \
  sudo chown -R dictation:dictation /srv/dictation/lessons && \
  sudo chmod -R g+rX /srv/dictation/lessons'
```

`/srv/dictation/data/` 留空，服务启动时自动建 SQLite 表。
`/srv/dictation/cache/` 只在 SBC 上生成音频时才需要。

### 3.6 SBC 上生成课时（可选）

如果要在 SBC 上直接 `gen`：

```bash
sudo apt install -y ffmpeg
```

生成课时的用户（你/agent）不需要写 `/opt`；只需要：

- 一个自己的 env 文件（含 TTS key）；
- `dictation` 组权限（写 `lessons/` 和 `cache/`）；
- `DICTATION_CACHE_DIR` 指到可写目录。

```bash
# 以你自己的用户执行（不要 sudo）
mkdir -p ~/.config/dictation
install -m 600 /tmp/dictation-env.example ~/.config/dictation/env
${EDITOR:-vi} ~/.config/dictation/env      # 填 DOUBAO_API_KEY

set -a
. ~/.config/dictation/env
set +a
umask 0002

# CLI 默认读取 /etc/dictation/config.toml；不在默认位置才需要 DICTATION_CONFIG
printf '%s' '{
  "title":"第5课",
  "text":"摇晃\n粗壮\n",
  "toml":"rate = 0.9\ngap = \"6s\"\n"
}' | /opt/dictation/dictation lesson import --payload - --json

/opt/dictation/dictation lesson gen \
  --title "第5课" --provider doubao --full-track
```

> 大多数情况不需要在 SBC 上生成：开发机生成、同步 `/srv/dictation/lessons/` 更省事，
> SBC 也不用装 key/ffmpeg。若采用这种方式，agent 在开发机用项目里的 `.env` 即可。

---

## 4. systemd

仓库里已提供 [`deploy/dictation.service`](deploy/dictation.service)。

### 4.1 安装 config 与 unit

`/etc/dictation/config.toml` 是应用自己的路径/server 配置（不含密钥，权限 644）：

```bash
sudo install -d -m 0755 /etc/dictation
sudo install -m 0644 /tmp/dictation-config.example /etc/dictation/config.toml
sudoedit /etc/dictation/config.toml
```

默认内容：

```toml
[paths]
base = "/srv/dictation"
lessons = "lessons"
data = "data"
cache = "cache"
web = "/opt/dictation/web"

[server]
host = "0.0.0.0"
port = 8080
```

检查解析结果：

```bash
/opt/dictation/dictation --config /etc/dictation/config.toml config show --json
```

> 如果改了 `base`，unit 里的 `ReadWritePaths=/srv/dictation` 也要同步改，
> 否则 `ProtectSystem=strict` 会阻止写入。

安装 unit：

```bash
sudo cp /tmp/dictation.service /etc/systemd/system/dictation.service
sudo systemctl daemon-reload
sudo systemctl enable --now dictation
```

unit 默认：

- 用户/组：`dictation`
- 工作目录：`/opt/dictation`
- 配置：`--config /etc/dictation/config.toml`
- 程序/前端：`/opt/dictation/dictation`、`/opt/dictation/web`
- 课时/数据/缓存：由 config 的 `paths` 决定（默认 `/srv/dictation/...`）
- `EnvironmentFile=-/etc/dictation/env`（可选；`serve` 不需要 key）
- `ProtectSystem=strict`，只允许写 `/srv/dictation`

### 4.2 可选：API key 环境文件

`/etc/dictation/env` 是给 **systemd 服务**的；`serve` 本身不需要 TTS key。
只有在服务进程里也要生成音频时才需要它。若你以普通用户/agent 在 SBC 上生成，
用 `~/.config/dictation/env`（见 3.6），不要和服务的 env 混用。

```bash
sudo install -d -m 0750 /etc/dictation
sudo install -m 0600 /tmp/dictation-env.example /etc/dictation/env
sudoedit /etc/dictation/env
```

> systemd `EnvironmentFile` 只接受 `KEY=VALUE`，不要写 `export`。
> 文件权限建议 `root:root 0600`；systemd 会在降权前读取。

### 4.3 启动/检查

```bash
sudo systemctl status dictation --no-pager
journalctl -u dictation -f

curl http://127.0.0.1:8080/api/health
curl http://127.0.0.1:8080/api/lessons
```

手机访问：

```bash
ip -4 addr show        # 找 SBC 的局域网 IP
# http://<SBC-IP>:8080
```

### 4.4 防火墙

如果 SBC 开了 ufw：

```bash
sudo ufw allow 8080/tcp
```

**没有账号/鉴权，只允许家庭局域网访问，不要做公网端口映射。**

---

## 5. 生成 / 同步课时

### 5.1 直接 gen（人类/开发机）

```bash
set -a; source .env; set +a
dictation gen lesson.txt --provider doubao --out lessons/第5课 --full-track
```

### 5.2 agent 推荐：CLI 导入

agent 不需要读写主机目录，只把内容通过 stdin 交给 CLI：

```bash
# 在开发机/或 SBC 上，配置由 /etc/dictation/config.toml 或 DICTATION_CONFIG 决定
printf '%s' '{
  "title":"第5课 词语听写",
  "text":"摇晃\n粗壮\n毕竟\n",
  "toml":"rate = 0.9\ngap = \"6s\"\nrepeat = 2\n"
}' | dictation lesson import --payload - --json

dictation lesson plan --title "第5课 词语听写" --json
dictation lesson gen --title "第5课 词语听写" --provider doubao --full-track
```

- `lesson import` 只写 `lesson.txt` / `lesson.toml`，不调 TTS；返回实际目录和预计时长。
- 已存在时加 `--force` 才覆盖；覆盖后要重新 `lesson gen`。
- `lesson list --json` 可查已有课时。
- 完整 payload 格式见 [`skills/dictation-prep/SKILL.md`](skills/dictation-prep/SKILL.md)。

### 5.3 同步到 SBC

开发机生成后：

```bash
rsync -av lessons/ k1:/tmp/dictation-lessons/
ssh k1 'sudo mkdir -p /srv/dictation/lessons && \
  sudo cp -a /tmp/dictation-lessons/. /srv/dictation/lessons/ && \
  sudo chown -R dictation:dictation /srv/dictation/lessons && \
  sudo chmod -R g+rX /srv/dictation/lessons'
```

- 服务每次 `GET /api/lessons` 都会重新扫描 `/srv/dictation/lessons/`，**不需要重启**。
- 手机端刷新首页即可看到新课时。
- `full.mp3` 可选；PWA 播放的是 `clips/*.wav`。

---

## 6. 安装 agent skill

skill 文件：[`skills/dictation-prep/SKILL.md`](skills/dictation-prep/SKILL.md)。
作用：把老师消息/课本照片/杂乱列表归一化成 JSON payload，交给
`dictation lesson import` 写入配置目录；确认后再 `dictation lesson gen`。
agent 不需要主机目录读写权限。

### OpenCode 项目级

```bash
mkdir -p .opencode/skills/dictation-prep
cp skills/dictation-prep/SKILL.md .opencode/skills/dictation-prep/
```

### OpenCode 全局

```bash
mkdir -p ~/.config/opencode/skills/dictation-prep
cp skills/dictation-prep/SKILL.md ~/.config/opencode/skills/dictation-prep/
```

> 不同 OpenCode 版本的 skills 目录可能不同；以你本机实际目录为准。

### 其他 agent

- 把 `SKILL.md` 放进该 agent 的 skills/instructions 目录；
- 或直接把内容粘进自定义指令/系统提示。

### 确保 `dictation` 在 PATH

```bash
sudo ln -sf /opt/dictation/dictation /usr/local/bin/dictation
dictation --version
```

如果不想装到 PATH，把 skill 里的 `dictation ...` 改成
`cargo run -- ...`（在项目根目录执行）。

安装后对 agent 说「准备听写」或「整理词表」即可触发。
skill 会先 `lesson import` 写词表/设置，再把预计时长给你确认；
确认后才 `lesson gen` 调 TTS。

### 在 SBC 上让 agent 生成

前提：agent 能执行主机上的 `dictation` 命令。推荐在开发机生成后同步；
如果确实要让 SBC 上的 agent 生成：

```bash
# 你的用户（不是 dictation）执行
id          # 应看到 dictation 组；没有就 usermod -aG 后重新登录
umask 0002
set -a; . ~/.config/dictation/env; set +a
export DICTATION_CONFIG=/etc/dictation/config.toml   # 如果不是默认位置
```

skill 通过 CLI 把内容写进 config 的 `lessons` 目录（默认
`/srv/dictation/lessons`），并 `gen` 到同一目录。
服务会扫描到新 `manifest.json`，**不需要重启**。

`data/` 不要给 agent 写：SQLite 由服务独占。
如果沙箱**完全不能执行主机 CLI**，只能在主机侧执行；不要用文件工具绕过。

---

## 7. 验证清单

```bash
dictation --version
dictation serve --help

curl http://127.0.0.1:8080/api/health
curl http://127.0.0.1:8080/api/lessons
```

手机端：

1. 打开 `http://<IP>:8080`
2. 首页能看到课时
3. 播放 → 暂停/继续/重听
4. 完成 → 检查 → 标错 → 提交
5. 首页出现「错词复习」
6. 复习标对后入口消失

---

## 8. 升级 / 备份

### 升级二进制

```bash
# 开发机重新构建
cargo zigbuild --release --target riscv64gc-unknown-linux-gnu

# SBC
sudo systemctl stop dictation
sudo install -Dm755 /tmp/dictation /opt/dictation/dictation
sudo systemctl start dictation
```

数据库表用 `CREATE TABLE IF NOT EXISTS`，当前没有迁移步骤；升级前建议备份。

### 更新前端

```bash
rsync -av web/ k1:/tmp/dictation-web/
ssh k1 'sudo cp -r /tmp/dictation-web/* /opt/dictation/web/'
```

静态文件按请求读取，**不用重启**。

### 备份

```bash
# 停服务后复制，或使用 sqlite3 .backup
sudo systemctl stop dictation
sudo cp /srv/dictation/data/dictation.db /path/to/backup/
sudo systemctl start dictation

# 课时与缓存
sudo tar czf dictation-lessons-$(date +%F).tgz -C /srv/dictation lessons
```

`/srv/dictation/data/dictation.db` 是错词/复习状态；
`/srv/dictation/lessons/` 是音频与词表；`/srv/dictation/cache/` 可重建。

---

## 9. 故障排查

| 现象 | 排查 |
|---|---|
| `GLIBC_2.30 not found` | SBC glibc 太老；用 `.2.28` 后缀重编 |
| `/api/lessons` 为空 | `/srv/dictation/lessons/*/manifest.json` 是否存在；权限；刷新首页 |
| 音频 404 | `manifest.json` 里的 `clips/...` 是否存在；`/srv/dictation/lessons` 权限 |
| agent 写不进 lessons | 你的用户是否在 `dictation` 组（`id`）；加组后重新登录；目录是否 2775；`umask 0002` |
| 服务读不到 lessons | `namei -l /srv/dictation/lessons`；组读位；`sudo -u dictation test -r ...` |
| `data/dictation.db` 权限错误 | `sudo chown -R dictation:dictation /srv/dictation/data`；目录 0750 |
| 8080 被占用 | `ss -ltnp \| grep 8080`；改 `--port` |
| `gen` 报缺 key | `set -a; source .env; set +a`；SBC 上用 `~/.config/dictation/env` |
| `ffmpeg not found` | 只影响 `full.mp3`/Qwen 变速；PWA 播放 WAV 不受影响 |
| systemd 起不来 | `journalctl -u dictation -n 100 --no-pager`；检查 unit 路径/ReadWritePaths |
| 手机打不开 | 同一 WiFi、SBC IP、防火墙；局域网 HTTP 可用，无 Service Worker |
| 页面还是旧版 | 强制刷新；当前没有 Service Worker 缓存 |

---

## 10. 安全

- 服务**没有账号/鉴权**，只适合家庭局域网；不要暴露到公网。
- API key 放 `.env`、`~/.config/dictation/env` 或 `/etc/dictation/env`，权限 600，不提交 git。
- `/srv/dictation/data/` 有孩子听写对错记录；`/srv/dictation/lessons/` 有词表与音频；定期备份。
- 拍照批改尚未实现；将来照片默认本地存，上传云端需显式开关。

---

## 11. 卸载

```bash
sudo systemctl disable --now dictation
sudo rm /etc/systemd/system/dictation.service
sudo systemctl daemon-reload
# 先备份再删除
sudo rm -rf /opt/dictation /srv/dictation /etc/dictation
sudo rm -f /usr/local/bin/dictation
```

删除前先备份 `/srv/dictation/lessons/` 和 `/srv/dictation/data/dictation.db`。
