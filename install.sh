#!/usr/bin/env bash
#
# dictation installer — Linux x86_64 / riscv64 SBC
#
# 从 GitHub release（或镜像目录）下载预编译包，按 INSTALL.md 的约定安装：
#   程序  /opt/dictation            (root:root, 只读)
#   数据  /srv/dictation/{lessons,data,cache} (dictation:dictation)
#   配置  /etc/dictation/config.toml, /etc/dictation/env
#   unit  /etc/systemd/system/dictation.service
#
# 幂等：重复运行会更新二进制/前端/managed unit 并重启服务；
# 已有的 config/env 和 lessons/data/cache 内容不会被覆盖或删除。
#
# 用法示例：
#   curl -fsSL https://github.com/OWNER/REPO/releases/download/v0.1.0/install.sh \
#     | sudo bash -s -- --version v0.1.0
#   sudo ./install.sh --version v0.1.0
#   sudo ./install.sh --base-url http://192.168.1.10:8000 --version v0.1.0
#   ./install.sh --root /tmp/dictation-test --no-systemd --version v0.1.0
#
set -euo pipefail

REPO_DEFAULT="__REPO__"
# 拼接写法：避免发布流程替换 __REPO__ 时把占位符检测本身也替换掉。
REPO_PLACEHOLDER="__REPO""__"
TAG=""
REPO="${DICTATION_REPO:-${REPO_DEFAULT}}"
BASE_URL="${DICTATION_BASE_URL:-}"
NO_SYSTEMD=0
DRY_RUN=0
ROOT_PREFIX=""
DATA_ROOT="/srv/dictation"
# 服务实际使用的数据根：优先取已存在 config.toml 的 [paths].base；
# 只有新装配置时才由 --data-root 决定。install_config 之后解析。
EFFECTIVE_BASE=""
DATA_DIR=""
TMP_DIR=""

log()  { printf '==> %s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die()  { printf 'error: %s\n' "$*" >&2; exit 1; }

usage() {
  cat <<'USAGE'
dictation installer

用法:
  install.sh [选项]

选项:
  --version <tag>       指定 release tag（如 v0.1.0）；默认取最新稳定版
  --repo <owner/repo>   GitHub 仓库；默认已由发布流程写入
  --base-url <url>      从镜像/本地 HTTP 目录下载（目录内需有资产和 SHA256SUMS）
  --no-systemd          只安装文件，不操作 systemd
  --dry-run             只打印将要执行的操作，不写安装目标（不要求 root）
  --root <dir>          安装到自定义前缀：<dir>/opt、<dir><data-root>、<dir>/etc；
                        同时跳过用户/组和 systemd（用于测试）
  --data-root <dir>     可写数据根目录，默认 /srv/dictation；只对“新装配置”生效。
                        若 /etc/dictation/config.toml 已存在，其 [paths].base 优先，
                        unit 的 ReadWritePaths 会跟随配置（不会改写已有配置）
                        也可以设为 /var/lib/dictation
  -h, --help            显示本帮助

环境变量:
  DICTATION_BASE_URL    同 --base-url
  DICTATION_REPO        同 --repo

说明:
  - 需要 root（--root <dir> 或 --dry-run 除外）。
  - 默认安装最新稳定版；预发布版本请显式 --version。
  - 已存在的 config.toml / env / 非 managed unit / lessons 数据不会被覆盖。
USAGE
}

require_value() {
  [ -n "${2:-}" ] || die "$1 需要参数"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version)      require_value "$1" "${2:-}"; TAG="$2"; shift 2 ;;
    --version=*)    TAG="${1#*=}"; shift ;;
    --repo)         require_value "$1" "${2:-}"; REPO="$2"; shift 2 ;;
    --repo=*)       REPO="${1#*=}"; shift ;;
    --base-url)     require_value "$1" "${2:-}"; BASE_URL="$2"; shift 2 ;;
    --base-url=*)   BASE_URL="${1#*=}"; shift ;;
    --no-systemd)   NO_SYSTEMD=1; shift ;;
    --dry-run)      DRY_RUN=1; shift ;;
    --root)         require_value "$1" "${2:-}"; ROOT_PREFIX="$2"; shift 2 ;;
    --root=*)       ROOT_PREFIX="${1#*=}"; shift ;;
    --data-root)    require_value "$1" "${2:-}"; DATA_ROOT="$2"; shift 2 ;;
    --data-root=*)  DATA_ROOT="${1#*=}"; shift ;;
    -h|--help)      usage; exit 0 ;;
    *)              die "未知参数: $1（用 --help 查看用法）" ;;
  esac
done

if [ -n "$ROOT_PREFIX" ]; then
  case "$ROOT_PREFIX" in
    /*) ;;
    *) die "--root 需要绝对路径: $ROOT_PREFIX" ;;
  esac
  ROOT_PREFIX="${ROOT_PREFIX%/}"
  [ -n "$ROOT_PREFIX" ] || die "--root 不能是 /"
fi

DATA_ROOT="${DATA_ROOT%/}"
case "$DATA_ROOT" in
  /) die "--data-root 不能是 /" ;;
  /*) ;;
  *) die "--data-root 需要绝对路径: $DATA_ROOT" ;;
esac
case "$DATA_ROOT" in
  *"/../"*|*"/.."|*" "*|*$'\t'*) die "--data-root 不能包含 .. 或空白: $DATA_ROOT" ;;
esac
[[ "$DATA_ROOT" =~ ^/[A-Za-z0-9._/-]+$ ]] \
  || die "--data-root 只支持字母、数字、._/-: $DATA_ROOT"

if [ "$DRY_RUN" -eq 0 ] && [ -z "$ROOT_PREFIX" ] && [ "$(id -u)" -ne 0 ]; then
  die "安装需要 root；请用 sudo 运行，或先用 --root <dir> / --dry-run 预览"
fi

# --root 模式用于测试/自定义前缀：不碰真实用户和 systemd。
if [ -n "$ROOT_PREFIX" ]; then
  NO_SYSTEMD=1
fi

OPT_DIR="$ROOT_PREFIX/opt/dictation"
# DATA_DIR 依赖 EFFECTIVE_BASE（install_config 之后才能确定），见 resolve_effective_base。
ETC_DIR="$ROOT_PREFIX/etc/dictation"
UNIT_DIR="$ROOT_PREFIX/etc/systemd/system"
UNIT_FILE="$UNIT_DIR/dictation.service"
UNIT_MARKER="# Managed by dictation installer"
HEALTH_URL="http://127.0.0.1:8080/api/health"

cleanup() {
  if [ -n "${TMP_DIR:-}" ] && [ -d "$TMP_DIR" ]; then
    rm -rf -- "$TMP_DIR" || true
  fi
}
trap cleanup EXIT INT TERM

run() {
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '[dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

# 数据根必须是绝对路径、不含 .. 和空白，且字符集受限（与 --data-root 一致）。
validate_base_path() {
  local p="$1"
  [ -n "$p" ] || return 1
  case "$p" in
    /) return 1 ;;
    /*) ;;
    *) return 1 ;;
  esac
  case "$p" in
    *"/../"*|*"/.."|*" "*|*$'\t'*) return 1 ;;
  esac
  [[ "$p" =~ ^/[A-Za-z0-9._/-]+$ ]] || return 1
  return 0
}

# parse_config_base <config.toml>
# 打印 [paths] 段里的 base 值；解析不到则打印空。
# 容错：CRLF、注释、单/双引号、行尾空白、base 位于同段其他键之后。
parse_config_base() {
  local file="$1"
  [ -f "$file" ] || return 0
  awk -v sq="'" '
    function trim(s) { sub(/^[ \t\r]+/, "", s); sub(/[ \t\r]+$/, "", s); return s }
    function unquote(v,   q, i) {
      v = trim(v)
      q = substr(v, 1, 1)
      if (q == "\"" || q == sq) {
        v = substr(v, 2)
        i = index(v, q)
        if (i > 0) v = substr(v, 1, i - 1)
        return trim(v)
      }
      sub(/[ \t]*#.*$/, "", v)
      return trim(v)
    }
    {
      sub(/\r$/, "")
      t = trim($0)
    }
    t == "" || t ~ /^#/ { next }
    t ~ /^\[/ {
      sect = t
      sub(/^\[[ \t]*/, "", sect)
      sub(/[ \t]*\].*$/, "", sect)
      sect = trim(sect)
      next
    }
    # 只认顶层或 [paths] 段；其他段的 base 忽略。
    sect != "" && sect != "paths" { next }
    t ~ /^base[ \t]*=/ {
      v = t
      sub(/^base[ \t]*=[ \t]*/, "", v)
      v = unquote(v)
      if (v != "") print v
    }
  ' "$file" 2>/dev/null | tail -n1
}

group_exists() {
  if command -v getent >/dev/null 2>&1; then
    getent group dictation >/dev/null 2>&1
  else
    grep -q '^dictation:' /etc/group 2>/dev/null
  fi
}

fetch() {
  # fetch <url> <dest>
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 --connect-timeout 15 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    die "需要 curl 或 wget 下载 release 资产"
  fi
}

fetch_optional() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 1 --connect-timeout 10 -o "$2" "$1" >/dev/null 2>&1
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1" >/dev/null 2>&1
  else
    return 1
  fi
}

validate_tag() {
  local tag="$1"
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.]*)?$ ]] \
    || die "版本格式不合法: '$tag'（期望 vX.Y.Z 或 vX.Y.Z-rc.N）"
}

resolve_latest() {
  local json tag
  if [ -n "$BASE_URL" ]; then
    if fetch_optional "${BASE_URL%/}/latest" "$TMP_DIR/latest"; then
      tag="$(tr -d '[:space:]' < "$TMP_DIR/latest")"
      [ -n "$tag" ] || die "镜像的 latest 文件为空; 请用 --version 指定版本"
      printf '%s\n' "$tag"
      return 0
    fi
    die "无法从镜像确定最新版本（缺少 ${BASE_URL%/}/latest）；请加 --version"
  fi

  if ! command -v curl >/dev/null 2>&1 && ! command -v wget >/dev/null 2>&1; then
    die "需要 curl 或 wget 查询 GitHub release"
  fi
  json="$TMP_DIR/release.json"
  if ! fetch "https://api.github.com/repos/${REPO}/releases/latest" "$json"; then
    die "无法查询 ${REPO} 的最新 release（网络或 GitHub API 限流）；请用 --version 指定"
  fi
  tag="$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$json" | head -n1)"
  [ -n "$tag" ] || die "GitHub API 未返回 tag_name；请用 --version 指定"
  printf '%s\n' "$tag"
}

case "$REPO" in
  "$REPO_PLACEHOLDER") die "install.sh 中的仓库占位符未替换；请加 --repo owner/repo" ;;
esac
[[ "$REPO" =~ ^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$ ]] \
  || die "--repo 格式不合法: '$REPO'（期望 owner/repo）"

if [ -n "$BASE_URL" ]; then
  case "$BASE_URL" in
    http://*|https://*|file://*) ;;
    *) die "--base-url 只支持 http://、https:// 或 file://: $BASE_URL" ;;
  esac
  case "$BASE_URL" in
    *[[:space:]]*) die "--base-url 不能包含空白字符" ;;
  esac
  BASE_URL="${BASE_URL%/}"
fi

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) TARGET="x86_64-unknown-linux-gnu" ;;
  Linux:riscv64)            TARGET="riscv64gc-unknown-linux-gnu" ;;
  *) die "不支持的平台: $(uname -s)/$(uname -m)（仅支持 Linux x86_64 / riscv64）" ;;
esac

if [ -z "$TAG" ]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/dictation-install.XXXXXX")"
  TAG="$(resolve_latest)"
fi
validate_tag "$TAG"

ASSET="dictation-${TAG}-${TARGET}.tar.gz"

log "版本:   $TAG"
log "仓库:   $REPO"
log "平台:   $TARGET"
if [ -n "$BASE_URL" ]; then
  log "来源:   $BASE_URL"
else
  log "来源:   GitHub releases"
fi
if [ "$DRY_RUN" -eq 1 ]; then
  log "模式:   dry-run（不写安装目标）"
fi

if [ -z "$TMP_DIR" ]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/dictation-install.XXXXXX")"
fi

if [ -n "$BASE_URL" ]; then
  ASSET_URL="${BASE_URL}/${ASSET}"
  SUMS_URL="${BASE_URL}/SHA256SUMS"
else
  ASSET_URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET}"
  SUMS_URL="https://github.com/${REPO}/releases/download/${TAG}/SHA256SUMS"
fi

log "下载 $ASSET"
fetch "$ASSET_URL" "$TMP_DIR/$ASSET"
fetch "$SUMS_URL" "$TMP_DIR/SHA256SUMS"
[ -s "$TMP_DIR/$ASSET" ] || die "下载的资产为空: $ASSET"

# ---- 校验 SHA256 ----
expected="$(awk -v f="$ASSET" '
  {
    p = $2
    sub(/^\.\//, "", p)
    if (p == f || p == "*" f) { print $1; exit }
  }
' "$TMP_DIR/SHA256SUMS")"
[ -n "$expected" ] || die "SHA256SUMS 中没有 $ASSET；拒绝安装"

if command -v sha256sum >/dev/null 2>&1; then
  printf '%s  %s\n' "$expected" "$ASSET" \
    | (cd "$TMP_DIR" && sha256sum -c -) \
    || die "SHA256 校验失败；资产可能损坏或被篡改"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$TMP_DIR/$ASSET" | awk '{ print $1 }')"
  [ "$actual" = "$expected" ] || die "SHA256 校验失败；资产可能损坏或被篡改"
  log "SHA256 校验通过（shasum）"
elif command -v openssl >/dev/null 2>&1; then
  actual="$(openssl dgst -sha256 -r "$TMP_DIR/$ASSET" | awk '{ print $1 }')"
  [ "$actual" = "$expected" ] || die "SHA256 校验失败；资产可能损坏或被篡改"
  log "SHA256 校验通过（openssl）"
elif command -v python3 >/dev/null 2>&1; then
  actual="$(python3 - "$TMP_DIR/$ASSET" <<'PY'
import hashlib
import sys
h = hashlib.sha256()
with open(sys.argv[1], "rb") as fh:
    for chunk in iter(lambda: fh.read(1024 * 1024), b""):
        h.update(chunk)
print(h.hexdigest())
PY
)"
  [ "$actual" = "$expected" ] || die "SHA256 校验失败；资产可能损坏或被篡改"
  log "SHA256 校验通过（python3）"
else
  die "找不到 sha256sum/shasum/openssl/python3，无法校验资产；请安装 coreutils"
fi

# ---- 归档安全检查 ----
listing="$TMP_DIR/listing.txt"
tar -tzf "$TMP_DIR/$ASSET" > "$listing" || die "无法读取 tar.gz: $ASSET"
while IFS= read -r entry; do
  case "$entry" in
    /*) die "归档包含绝对路径，拒绝安装: $entry" ;;
    ..|../*|*/..|*/../*) die "归档包含 .. 路径，拒绝安装: $entry" ;;
    "dictation-${TAG}-${TARGET}"|"dictation-${TAG}-${TARGET}"/*) ;;
    *) die "归档顶层目录异常，拒绝安装: $entry" ;;
  esac
done < "$listing"

log "解压"
mkdir -p "$TMP_DIR/pkg"
tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR/pkg" --no-same-owner
SRC="$TMP_DIR/pkg/dictation-${TAG}-${TARGET}"
[ -x "$SRC/dictation" ] || die "包中缺少可执行文件: dictation"
[ -f "$SRC/web/index.html" ] || die "包中缺少前端: web/index.html"
[ -f "$SRC/deploy/dictation.service" ] || die "包中缺少 deploy/dictation.service"
[ -f "$SRC/deploy/config.example" ] || die "包中缺少 deploy/config.example"
[ -f "$SRC/deploy/env.example" ] || die "包中缺少 deploy/env.example"
[ -f "$SRC/INSTALL.md" ] || die "包中缺少 INSTALL.md"
[ -f "$SRC/README.md" ] || die "包中缺少 README.md"

# ---- 用户/组 ----
ensure_user() {
  if [ -n "$ROOT_PREFIX" ]; then
    log "跳过用户/组（--root 模式）"
    return 0
  fi
  if group_exists; then
    log "组 dictation 已存在"
  else
    run groupadd --system dictation
  fi
  if id -u dictation >/dev/null 2>&1; then
    log "用户 dictation 已存在"
  else
    [ -n "$EFFECTIVE_BASE" ] || die "内部错误：EFFECTIVE_BASE 未初始化"
    run useradd --system --gid dictation --home "$EFFECTIVE_BASE" \
      --shell /usr/sbin/nologin dictation
  fi
  if [ -n "${SUDO_USER:-}" ] && [ "${SUDO_USER}" != "root" ]; then
    if id -nG "$SUDO_USER" 2>/dev/null | tr ' ' '\n' | grep -qx dictation; then
      log "用户 ${SUDO_USER} 已在 dictation 组"
    else
      run usermod -aG dictation "$SUDO_USER"
      warn "已将 ${SUDO_USER} 加入 dictation 组；请重新登录后生效（id 应能看到 dictation）"
    fi
  fi
}

copy_tree() {
  # copy_tree <src-dir> <dest-dir>
  local src="$1" dest="$2"
  [ -d "$src" ] || die "包中缺少目录: $src"
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '[dry-run] install -d -m 0755 %s && cp -a %s/. %s/\n' "$dest" "$src" "$dest"
  else
    install -d -m 0755 "$dest"
    cp -a "$src/." "$dest/"
  fi
}

install_files() {
  log "安装程序到 $OPT_DIR"
  run install -d -m 0755 "$OPT_DIR"
  # 先写临时文件再原子替换，避免覆盖正在运行的二进制。
  run install -m 0755 "$SRC/dictation" "$OPT_DIR/dictation.new"
  run mv -f "$OPT_DIR/dictation.new" "$OPT_DIR/dictation"

  copy_tree "$SRC/web" "$OPT_DIR/web"
  copy_tree "$SRC/deploy" "$OPT_DIR/deploy"
  copy_tree "$SRC/docs" "$OPT_DIR/docs"
  copy_tree "$SRC/skills" "$OPT_DIR/skills"
  run install -m 0644 "$SRC/INSTALL.md" "$OPT_DIR/INSTALL.md"
  run install -m 0644 "$SRC/README.md" "$OPT_DIR/README.md"
  [ -f "$SRC/LICENSE" ] && run install -m 0644 "$SRC/LICENSE" "$OPT_DIR/LICENSE"

  if [ -n "$ROOT_PREFIX" ]; then
    log "跳过 chown（--root 模式）；设置程序目录只读权限"
    run chmod -R go-w "$OPT_DIR"
  else
    run chown -R root:root "$OPT_DIR"
    run chmod -R go-w "$OPT_DIR"
  fi
  run chmod 0755 "$OPT_DIR"
}

install_config() {
  log "安装配置到 $ETC_DIR"
  run install -d -m 0755 "$ETC_DIR"

  if [ -e "$ETC_DIR/config.toml" ]; then
    log "保留现有配置: $ETC_DIR/config.toml"
  else
    if [ "$DATA_ROOT" = "/srv/dictation" ]; then
      run install -m 0644 "$SRC/deploy/config.example" "$ETC_DIR/config.toml"
    else
      # 替换整行 base（容忍引号/空白变化），而不是只匹配默认值。
      sed -E "s|^[[:space:]]*base[[:space:]]*=.*\$|base = \"$DATA_ROOT\"|" \
        "$SRC/deploy/config.example" > "$TMP_DIR/config.toml"
      run install -m 0644 "$TMP_DIR/config.toml" "$ETC_DIR/config.toml"
    fi
    log "已安装 $ETC_DIR/config.toml（请按需编辑）"
  fi

  if [ -e "$ETC_DIR/env" ]; then
    log "保留现有 env: $ETC_DIR/env"
  else
    run install -m 0600 "$SRC/deploy/env.example" "$ETC_DIR/env"
    log "已安装 $ETC_DIR/env（权限 0600；serve 不需要 key）"
  fi
}

# 确定服务真正会用的数据根：已有配置的 [paths].base 优先于 --data-root。
# 之后 unit 的 ReadWritePaths、数据目录、useradd --home、汇总都基于它，
# 保证 unit 与 config 永不打架（ProtectSystem=strict 下写不进去）。
resolve_effective_base() {
  local cfg="$ETC_DIR/config.toml" parsed=""

  EFFECTIVE_BASE="$DATA_ROOT"

  if [ -e "$cfg" ]; then
    parsed="$(parse_config_base "$cfg" || true)"
    if [ -z "$parsed" ]; then
      warn "无法从 $cfg 解析 [paths].base；回退到 --data-root $EFFECTIVE_BASE"
      warn "请手动确认 $cfg 的 [paths].base 与 unit 的 ReadWritePaths 一致"
    elif validate_base_path "$parsed"; then
      EFFECTIVE_BASE="$parsed"
      if [ "$parsed" != "$DATA_ROOT" ]; then
        warn "现有配置的 [paths].base = \"$parsed\" 与 --data-root $DATA_ROOT 不一致"
        warn "以现有配置为准：数据目录和 unit ReadWritePaths 都使用 $parsed（不改写配置）"
        warn "如需迁移到 $DATA_ROOT："
        warn "  1) systemctl stop dictation"
        warn "  2) rsync -aHAX --numeric-ids $parsed/ $DATA_ROOT/"
        warn "  3) 编辑 $cfg，把 [paths].base 改为 \"$DATA_ROOT\""
        warn "  4) 重新运行本安装脚本（省略 --data-root 即可）"
      fi
    else
      warn "配置中的 base 不可用（需绝对路径、无 .. 和空白）: '$parsed'"
      warn "回退到 --data-root $EFFECTIVE_BASE；请手动修正 $cfg"
    fi
  fi

  if [ -n "$ROOT_PREFIX" ]; then
    DATA_DIR="$ROOT_PREFIX$EFFECTIVE_BASE"
  else
    DATA_DIR="$EFFECTIVE_BASE"
  fi

  local src_desc="--data-root"
  if [ -e "$cfg" ]; then
    src_desc="配置 [paths].base"
  fi
  log "数据根: $EFFECTIVE_BASE（来自 $src_desc）"
}

install_unit() {
  log "安装 systemd unit"
  [ -n "$EFFECTIVE_BASE" ] || die "内部错误：EFFECTIVE_BASE 未初始化"
  local unit_src="$TMP_DIR/dictation.service"
  sed "s|/srv/dictation|$EFFECTIVE_BASE|g" "$SRC/deploy/dictation.service" > "$unit_src"

  if [ ! -e "$UNIT_FILE" ]; then
    run install -d -m 0755 "$UNIT_DIR"
    run install -m 0644 "$unit_src" "$UNIT_FILE"
    log "已安装 $UNIT_FILE"
    return 0
  fi

  local first
  first="$(head -n1 "$UNIT_FILE" 2>/dev/null || true)"
  case "$first" in
    *"$UNIT_MARKER"*)
      run install -m 0644 "$unit_src" "$UNIT_FILE"
      log "已更新 managed unit: $UNIT_FILE"
      ;;
    *)
      warn "保留已有 unit（非 dictation installer 管理）: $UNIT_FILE"
      ;;
  esac
}

install_data_dirs() {
  [ -n "$DATA_DIR" ] || die "内部错误：DATA_DIR 未初始化"
  log "准备数据目录 $DATA_DIR"
  run install -d -m 0755 "$DATA_DIR"
  run install -d -m 2775 "$DATA_DIR/lessons"
  run install -d -m 2775 "$DATA_DIR/cache"
  run install -d -m 0750 "$DATA_DIR/data"

  if [ -n "$ROOT_PREFIX" ]; then
    log "跳过数据目录 chown（--root 模式）"
  elif id -u dictation >/dev/null 2>&1; then
    run chown -R dictation:dictation "$DATA_DIR"
    run chmod 2775 "$DATA_DIR/lessons" "$DATA_DIR/cache"
    run chmod 0750 "$DATA_DIR/data"
  else
    warn "用户 dictation 不存在，跳过 chown；请手动修正 $DATA_DIR 属主"
  fi
}

systemd_available() {
  command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]
}

setup_systemd() {
  if [ "$NO_SYSTEMD" -eq 1 ]; then
    log "跳过 systemd（--no-systemd 或 --root）"
    return 0
  fi
  if ! systemd_available; then
    warn "systemd 不可用；文件已安装，但未启动服务"
    warn "稍后可运行: systemctl daemon-reload && systemctl enable --now dictation"
    return 0
  fi
  if [ "$DRY_RUN" -eq 1 ]; then
    printf '[dry-run] systemctl daemon-reload\n'
    printf '[dry-run] systemctl enable dictation\n'
    printf '[dry-run] systemctl restart dictation\n'
    return 0
  fi

  systemctl daemon-reload || warn "systemctl daemon-reload 失败"
  systemctl enable dictation || warn "systemctl enable dictation 失败"
  if ! systemctl restart dictation; then
    warn "systemctl restart dictation 失败；请查看: journalctl -u dictation -n 100 --no-pager"
  fi
}

health_check() {
  [ "$NO_SYSTEMD" -eq 0 ] || return 0
  [ "$DRY_RUN" -eq 0 ] || return 0
  systemd_available || return 0

  log "检查服务健康状态: $HEALTH_URL"
  for _ in $(seq 1 20); do
    if command -v curl >/dev/null 2>&1; then
      if curl -fsS --max-time 2 "$HEALTH_URL" >/dev/null 2>&1; then
        log "服务已就绪"
        return 0
      fi
    elif wget -q -O /dev/null --timeout=2 "$HEALTH_URL" >/dev/null 2>&1; then
      log "服务已就绪"
      return 0
    fi
    sleep 1
  done
  warn "健康检查失败: $HEALTH_URL"
  warn "安装文件已保留，未回滚。请检查: journalctl -u dictation -n 100 --no-pager"
  warn "确认 /etc/dictation/config.toml 的 paths.web 指向 /opt/dictation/web"
  return 0
}

install_files
install_config
resolve_effective_base
ensure_user
install_unit
install_data_dirs
setup_systemd
health_check

log "完成"
printf '\n'
printf '程序:   %s\n' "$OPT_DIR/dictation"
printf '配置:   %s\n' "$ETC_DIR/config.toml"
printf '数据根: %s\n' "$EFFECTIVE_BASE"
printf '数据:   %s\n' "$DATA_DIR"
printf '前端:   http://<本机IP>:8080\n'
printf '\n'
printf '下一步:\n'
printf '  - 编辑 %s 检查 paths/server\n' "$ETC_DIR/config.toml"
printf '  - 服务端生成需要 TTS key：编辑 %s 填入 DOUBAO_API_KEY（0600）\n' "$ETC_DIR/env"
printf '  - 查看状态: systemctl status dictation --no-pager\n'
printf '  - 健康检查: curl %s\n' "$HEALTH_URL"
