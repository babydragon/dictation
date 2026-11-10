use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use serde::Deserialize;
use serde_json::json;

use dictation::app_config::{self, AppConfig};
use dictation::cache::{self, Cache};
use dictation::config::Config;
use dictation::db::{Db, SessionResult, WrongWord};
use dictation::lesson;
use dictation::model::{Lang, Lesson, ProviderKind};
use dictation::plan::{self, Plan};
use dictation::render::{self, RenderOptions};
use dictation::result_view::{format_unix, item_json, session_json, wrong_json};
use dictation::tts;

#[derive(Debug, Parser)]
#[command(
    name = "dictation",
    version,
    about = "把纯文本词表生成精确控制节奏的听写音频"
)]
struct Cli {
    /// 配置文件路径（默认 /etc/dictation/config.toml，可用 DICTATION_CONFIG 指定）
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 只估算时间轴与总时长，不调用 TTS
    Plan {
        /// 纯文本词表路径
        lesson: PathBuf,
        /// 输出 JSON 而不是表格
        #[arg(long)]
        json: bool,
    },
    /// 合成片段、整轨与 manifest
    Gen {
        /// 纯文本词表路径
        lesson: PathBuf,
        /// 输出目录，默认 <lessons>/<标题>
        #[arg(long)]
        out: Option<PathBuf>,
        /// TTS provider，默认取 DICTATION_PROVIDER 或 qwen
        #[arg(long, value_name = "PROVIDER", value_parser = ["doubao", "qwen", "mock"])]
        provider: Option<String>,
        /// 额外导出 full.mp3
        #[arg(long)]
        full_track: bool,
        /// 覆盖中文音色
        #[arg(long, value_name = "VOICE")]
        voice_zh: Option<String>,
        /// 覆盖英文音色
        #[arg(long, value_name = "VOICE")]
        voice_en: Option<String>,
        /// 覆盖课时语速（0.5–2.0）
        #[arg(long, value_name = "RATE")]
        rate: Option<f32>,
        /// 覆盖词间间隔（6000 / 6000ms / 6s）
        #[arg(long, value_name = "DUR")]
        gap: Option<String>,
        /// 覆盖每词朗读遍数（1–10）
        #[arg(long, value_name = "N")]
        repeat: Option<u8>,
    },
    /// 生成新的 lesson.txt 模板
    New {
        /// 课时标题
        title: String,
        /// 输出目录，默认 <lessons>/<标题>
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 播放器与自检服务
    Serve {
        /// 课时根目录，默认取配置文件
        #[arg(long)]
        dir: Option<PathBuf>,
        /// 静态前端目录，默认取配置文件
        #[arg(long)]
        web: Option<PathBuf>,
        /// SQLite 文件路径，默认 <data>/dictation.db
        #[arg(long)]
        data: Option<PathBuf>,
        /// 监听地址，默认取配置文件
        #[arg(long)]
        host: Option<String>,
        /// 端口，默认取配置文件
        #[arg(long)]
        port: Option<u16>,
    },
    /// 显示解析后的配置
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// 按配置文件管理课时（agent 主入口）
    Lesson {
        #[command(subcommand)]
        command: LessonCommand,
    },
    /// 查询听写结果与错词（家长/agent 分析用）
    Result {
        #[command(subcommand)]
        command: ResultCommand,
    },
    /// 清理片段缓存（默认 dry-run，需 --apply 才删除）
    Clean {
        /// 只清理没有被任何课时引用的条目
        #[arg(long)]
        unused: bool,
        /// 只清理超过该时间的条目：30d / 12h / 45m / 30（纯数字按天）
        #[arg(long, value_name = "AGE")]
        older_than: Option<String>,
        /// 真正删除（默认只统计）
        #[arg(long)]
        apply: bool,
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ResultCommand {
    /// 最近听写列表（默认只列已提交检查的）
    List {
        /// 输出 JSON
        #[arg(long)]
        json: bool,
        /// 最多列出多少条
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// 包含还没提交检查的 session
        #[arg(long)]
        all: bool,
    },
    /// 某次听写的完整结果
    Show {
        /// session id；与 --latest 二选一
        session_id: Option<String>,
        /// 取最近一个已提交检查的 session
        #[arg(long)]
        latest: bool,
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
    /// 某次听写只列错词
    Wrong {
        /// session id；与 --latest 二选一
        session_id: Option<String>,
        /// 取最近一个已提交检查的 session
        #[arg(long)]
        latest: bool,
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// 输出配置来源与解析后的路径
    Show {
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
enum LessonCommand {
    /// 从 stdin/文件导入课时（JSON 或 flags），校验后才落盘
    Import {
        /// JSON 输入文件，`-` 表示 stdin（与 --title/--text/--toml/--dir 互斥）
        #[arg(long, value_name = "FILE|-")]
        payload: Option<String>,
        /// 课时标题（与 --text 搭配使用）
        #[arg(long)]
        title: Option<String>,
        /// 词表文件，`-` 表示 stdin
        #[arg(long, value_name = "FILE|-")]
        text: Option<String>,
        /// lesson.toml 文件，`-` 表示 stdin
        #[arg(long, value_name = "FILE|-")]
        toml: Option<String>,
        /// 目录名，默认由标题生成
        #[arg(long)]
        dir: Option<String>,
        /// 覆盖已存在的课时
        #[arg(long)]
        force: bool,
        /// 科目：chinese / english（必填；--payload 时从 payload 读取）
        #[arg(long, value_name = "SUBJECT")]
        subject: Option<String>,
        /// 年级 1–6（可选）
        #[arg(long, value_name = "1-6")]
        grade: Option<u8>,
        /// 学期 1=上 / 2=下（可选）
        #[arg(long, value_name = "1|2")]
        term: Option<u8>,
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
    /// 估算配置目录下某课时的时长
    Plan {
        /// 课时标题（也可用 --dir）
        #[arg(long)]
        title: Option<String>,
        /// 目录名
        #[arg(long)]
        dir: Option<String>,
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
    /// 合成配置目录下的课时
    Gen {
        /// 课时标题（也可用 --dir）
        #[arg(long)]
        title: Option<String>,
        /// 目录名
        #[arg(long)]
        dir: Option<String>,
        #[arg(long, value_name = "PROVIDER", value_parser = ["doubao", "qwen", "mock"])]
        provider: Option<String>,
        #[arg(long)]
        full_track: bool,
        #[arg(long, value_name = "VOICE")]
        voice_zh: Option<String>,
        #[arg(long, value_name = "VOICE")]
        voice_en: Option<String>,
        #[arg(long, value_name = "RATE")]
        rate: Option<f32>,
        #[arg(long, value_name = "DUR")]
        gap: Option<String>,
        #[arg(long, value_name = "N")]
        repeat: Option<u8>,
        /// 输出 JSON 错误
        #[arg(long)]
        json: bool,
    },
    /// 列出配置目录下已生成的课时
    List {
        /// 输出 JSON
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let app = AppConfig::load(cli.config)?;

    match cli.command {
        Command::Plan { lesson, json } => cmd_plan(&lesson, json),
        Command::Gen {
            lesson,
            out,
            provider,
            full_track,
            voice_zh,
            voice_en,
            rate,
            gap,
            repeat,
        } => {
            cmd_gen(
                &app, &lesson, out, provider, full_track, voice_zh, voice_en, rate, gap, repeat,
            )
            .await
        }
        Command::New { title, out } => cmd_new(&app, &title, out),
        Command::Serve {
            dir,
            web,
            data,
            host,
            port,
        } => {
            let options = dictation::server::ServeOptions {
                lessons_dir: dir.unwrap_or_else(|| app.lessons_dir.clone()),
                web_dir: web.unwrap_or_else(|| app.web_dir.clone()),
                data_path: data.unwrap_or_else(|| app.db_path()),
                cache_dir: app.cache_dir.clone(),
                host: host.unwrap_or_else(|| app.host.clone()),
                port: port.unwrap_or(app.port),
            };
            dictation::server::serve(options).await
        }
        Command::Config { command } => match command {
            ConfigCommand::Show { json } => cmd_config_show(&app, json),
        },
        Command::Lesson { command } => match command {
            LessonCommand::Import {
                payload,
                title,
                text,
                toml,
                dir,
                force,
                subject,
                grade,
                term,
                json,
            } => cmd_lesson_import(
                &app,
                ImportOptions {
                    payload,
                    title,
                    text,
                    toml,
                    dir,
                    force,
                    subject,
                    grade,
                    term,
                    json,
                },
            ),
            LessonCommand::Plan { title, dir, json } => {
                match cmd_lesson_plan(&app, title.as_deref(), dir.as_deref(), json) {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        if json {
                            exit_with_json_error(&err);
                        }
                        Err(err)
                    }
                }
            }
            LessonCommand::Gen {
                title,
                dir,
                provider,
                full_track,
                voice_zh,
                voice_en,
                rate,
                gap,
                repeat,
                json,
            } => {
                let result = async {
                    let source = lesson_source_path(&app, title.as_deref(), dir.as_deref())?;
                    let out = source.parent().map(Path::to_path_buf);
                    cmd_gen(
                        &app, &source, out, provider, full_track, voice_zh, voice_en, rate, gap,
                        repeat,
                    )
                    .await
                }
                .await;
                match result {
                    Ok(()) => Ok(()),
                    Err(err) => {
                        if json {
                            exit_with_json_error(&err);
                        }
                        Err(err)
                    }
                }
            }
            LessonCommand::List { json } => match cmd_lesson_list(&app, json) {
                Ok(()) => Ok(()),
                Err(err) => {
                    if json {
                        exit_with_json_error(&err);
                    }
                    Err(err)
                }
            },
        },
        Command::Result { command } => match command {
            ResultCommand::List { json, limit, all } => {
                with_json_errors(json, cmd_result_list(&app, json, limit, all))
            }
            ResultCommand::Show {
                session_id,
                latest,
                json,
            } => with_json_errors(
                json,
                cmd_result_show(&app, session_id.as_deref(), latest, json),
            ),
            ResultCommand::Wrong {
                session_id,
                latest,
                json,
            } => with_json_errors(
                json,
                cmd_result_wrong(&app, session_id.as_deref(), latest, json),
            ),
        },
        Command::Clean {
            unused,
            older_than,
            apply,
            json,
        } => with_json_errors(
            json,
            cmd_clean(&app, unused, older_than.as_deref(), apply, json),
        ),
    }
}

/// Print `{"error": ...}` on stderr (and exit 1) when `--json` is active.
fn with_json_errors(json: bool, result: Result<()>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(err) => {
            if json {
                exit_with_json_error(&err);
            }
            Err(err)
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

fn cmd_plan(path: &Path, json: bool) -> Result<()> {
    let lesson = lesson::parse_file(path)?;
    let plan = plan::estimate(&lesson);
    if json {
        print_plan_json(&lesson, &plan)?;
    } else {
        print_plan_table(&lesson, &plan);
    }
    Ok(())
}

fn print_plan_table(lesson: &Lesson, plan: &Plan) {
    println!("《{}》 共 {} 条", lesson.title, lesson.items.len());
    if let Some(text) = &plan.intro_text {
        println!(
            "开场白「{}」约 {} + 间隔 {}",
            truncate(text, 28),
            format_seconds(plan.intro_ms),
            format_seconds(plan.intro_gap_ms)
        );
    } else {
        println!("开场白：已关闭");
    }
    println!(
        "{:>4}  {:<16}  {:<4}  {:>9}  {:>8}  {:>9}",
        "序号", "文本", "语言", "预估音频", "间隔", "累计"
    );
    let mut cumulative =
        u64::from(plan.lead_in_ms) + u64::from(plan.intro_ms) + u64::from(plan.intro_gap_ms);
    for item in &plan.items {
        cumulative += u64::from(item.total_ms);
        let spoken = item.total_ms.saturating_sub(item.gap_ms);
        println!(
            "{:>4}  {:<16}  {:<4}  {:>9}  {:>8}  {:>9}",
            item.index,
            truncate(&item.text, 16),
            lang_label(item.lang),
            format_seconds(spoken),
            format_seconds(item.gap_ms),
            format_seconds(cumulative as u32),
        );
    }
    if let Some(text) = &plan.outro_text {
        println!(
            "结束语「{}」约 {}",
            truncate(text, 28),
            format_seconds(plan.outro_ms)
        );
    } else {
        println!("结束语：已关闭");
    }
    println!(
        "前导静音 {}，预计总时长 {}（{} ms）",
        format_seconds(plan.lead_in_ms),
        format_clock(plan.total_ms),
        plan.total_ms
    );
    if plan.total_ms > 20 * 60 * 1000 {
        println!("提示：总时长超过 20 分钟，建议拆分成两次听写。");
    }
}

fn print_plan_json(lesson: &Lesson, plan: &Plan) -> Result<()> {
    let items: Vec<serde_json::Value> = plan
        .items
        .iter()
        .map(|item| {
            serde_json::json!({
                "index": item.index,
                "text": item.text,
                "lang": format!("{:?}", item.lang).to_ascii_lowercase(),
                "announce_ms": item.announce_ms,
                "clip_ms": item.clip_ms,
                "repeat": item.repeat,
                "repeat_gap_ms": item.repeat_gap_ms,
                "gap_ms": item.gap_ms,
                "total_ms": item.total_ms,
            })
        })
        .collect();
    let value = serde_json::json!({
        "title": lesson.title,
        "source": lesson.source,
        "lead_in_ms": plan.lead_in_ms,
        "intro_ms": plan.intro_ms,
        "intro_gap_ms": plan.intro_gap_ms,
        "intro_text": plan.intro_text,
        "outro_ms": plan.outro_ms,
        "outro_text": plan.outro_text,
        "total_ms": plan.total_ms,
        "items": items,
    });
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn cmd_gen(
    app: &AppConfig,
    path: &Path,
    out: Option<PathBuf>,
    provider: Option<String>,
    full_track: bool,
    voice_zh: Option<String>,
    voice_en: Option<String>,
    rate: Option<f32>,
    gap: Option<String>,
    repeat: Option<u8>,
) -> Result<()> {
    let mut lesson = lesson::parse_file(path)?;

    if let Some(voice) = voice_zh {
        lesson.config.voice_zh = Some(voice);
    }
    if let Some(voice) = voice_en {
        lesson.config.voice_en = Some(voice);
    }
    if let Some(rate) = rate {
        if !(0.5..=2.0).contains(&rate) {
            bail!("--rate 必须在 0.5–2.0 之间，当前 {rate}");
        }
        lesson.config.rate = rate;
    }
    if let Some(gap) = gap {
        let gap_ms = lesson::parse_duration_ms(&gap)?;
        if gap_ms > 120_000 {
            bail!("--gap 不能超过 120s，当前 {gap_ms}ms");
        }
        lesson.config.gap_ms = gap_ms;
    }
    if let Some(repeat) = repeat {
        if !(1..=10).contains(&repeat) {
            bail!("--repeat 必须在 1–10 之间，当前 {repeat}");
        }
        lesson.config.repeat = repeat;
    }

    let config = runtime_config(app)?;
    let kind = match provider {
        Some(raw) => raw.parse::<ProviderKind>()?,
        None => config.default_provider,
    };
    let provider = tts::build_provider(kind, &config)?;

    let out_dir = out.unwrap_or_else(|| default_out_dir(app, &lesson.title));
    let cache = Cache::new(&config.cache_dir);
    let options = RenderOptions {
        out_dir: out_dir.clone(),
        full_track,
    };

    let result = render::render(&lesson, provider.as_ref(), &cache, &config, &options).await?;

    let clip_count = count_clips(&out_dir.join("clips"));
    println!("已生成《{}》", lesson.title);
    println!("  输出目录：{}", out_dir.display());
    println!("  片段文件：{clip_count} 个");
    println!(
        "  TTS 调用：{}，缓存命中：{}",
        result.tts_calls, result.cache_hits
    );
    println!(
        "  总时长：{}（{} ms）",
        format_clock(result.total_ms),
        result.total_ms
    );
    println!("  manifest：{}", result.manifest_path.display());
    if let Some(track) = &result.full_track {
        println!("  整轨：{}", track.display());
    }
    Ok(())
}

/// Environment config, with the cache directory falling back to the app config.
fn runtime_config(app: &AppConfig) -> Result<Config> {
    let mut config = Config::from_env()?;
    let cache_from_env = std::env::var("DICTATION_CACHE_DIR")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !cache_from_env {
        config.cache_dir = app.cache_dir.clone();
    }
    Ok(config)
}

fn cmd_new(app: &AppConfig, title: &str, out: Option<PathBuf>) -> Result<()> {
    let title = title.trim();
    if title.is_empty() {
        bail!("标题不能为空");
    }
    let dir = out.unwrap_or_else(|| default_out_dir(app, title));
    fs::create_dir_all(&dir).with_context(|| format!("创建目录失败: {}", dir.display()))?;
    let path = dir.join("lesson.txt");
    if path.exists() {
        bail!("{} 已存在，不会覆盖", path.display());
    }
    let template = format!(
        "# {title}\n\n# 一行一个词。可选行内属性：\n# 摇晃 | rate=0.9 gap=6s repeat=2\n# 长大 | speak=长大的长\n"
    );
    fs::write(&path, template).with_context(|| format!("写入失败: {}", path.display()))?;
    println!("已创建模板：{}", path.display());
    println!("把词表填进去后运行：dictation plan {}", path.display());
    Ok(())
}

fn default_out_dir(app: &AppConfig, title: &str) -> PathBuf {
    app.lessons_dir.join(app_config::safe_dir_name(title))
}

fn cmd_config_show(app: &AppConfig, json: bool) -> Result<()> {
    let db_path = app.db_path();
    if json {
        let value = json!({
            "source": app.source().map(|path| path.display().to_string()),
            "source_label": app.source_label(),
            "lessons": app.lessons_dir.display().to_string(),
            "data": app.data_dir.display().to_string(),
            "db": db_path.display().to_string(),
            "cache": app.cache_dir.display().to_string(),
            "web": app.web_dir.display().to_string(),
            "host": app.host,
            "port": app.port,
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!("配置来源：{}", app.source_label());
        println!("  lessons：{}", app.lessons_dir.display());
        println!("  data：{}", app.data_dir.display());
        println!("  db：{}", db_path.display());
        println!("  cache：{}", app.cache_dir.display());
        println!("  web：{}", app.web_dir.display());
        println!("  server：{}:{}", app.host, app.port);
    }
    Ok(())
}

#[derive(Debug, Default)]
struct ImportOptions {
    payload: Option<String>,
    title: Option<String>,
    text: Option<String>,
    toml: Option<String>,
    dir: Option<String>,
    force: bool,
    subject: Option<String>,
    grade: Option<u8>,
    term: Option<u8>,
    json: bool,
}

#[derive(Debug, Deserialize)]
struct ImportPayload {
    title: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    grade: Option<u8>,
    #[serde(default)]
    term: Option<u8>,
    #[serde(default)]
    dir: Option<String>,
    text: String,
    #[serde(default)]
    toml: Option<String>,
    #[serde(default)]
    force: bool,
}

fn cmd_lesson_import(app: &AppConfig, options: ImportOptions) -> Result<()> {
    let json = options.json;
    match import_lesson(app, options) {
        Ok(outcome) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "dir": outcome.dir_path.display().to_string(),
                        "title": outcome.title,
                        "subject": outcome.subject,
                        "grade": outcome.grade,
                        "term": outcome.term,
                        "item_count": outcome.item_count,
                        "total_ms": outcome.total_ms,
                        "created": outcome.created,
                    }))?
                );
            } else {
                println!(
                    "已{}《{}》",
                    if outcome.created { "导入" } else { "覆盖" },
                    outcome.title
                );
                println!("  目录：{}", outcome.dir_path.display());
                println!(
                    "  科目：{}{}",
                    outcome.subject,
                    match (outcome.grade, outcome.term) {
                        (Some(grade), Some(term)) => format!("  {grade} 年级 {term} 学期"),
                        (Some(grade), None) => format!("  {grade} 年级"),
                        _ => String::new(),
                    }
                );
                println!(
                    "  {} 条，预计 {}（{} ms）",
                    outcome.item_count,
                    format_clock(outcome.total_ms),
                    outcome.total_ms
                );
            }
            Ok(())
        }
        Err(err) => {
            if json {
                exit_with_json_error(&err);
            }
            Err(err)
        }
    }
}

/// Thin CLI wrapper around [`lesson::import::import`]; the validation and
/// file-writing logic is shared with the HTTP API.
fn import_lesson(app: &AppConfig, options: ImportOptions) -> Result<lesson::import::ImportOutcome> {
    let (title, dir_arg, text, toml_text, force, subject, grade, term) = match options
        .payload
        .as_deref()
    {
        Some(spec) => {
            if options.title.is_some()
                || options.text.is_some()
                || options.toml.is_some()
                || options.dir.is_some()
                || options.subject.is_some()
                || options.grade.is_some()
                || options.term.is_some()
            {
                bail!(
                        "--payload 不能与 --title/--text/--toml/--dir/--subject/--grade/--term 同时使用"
                    );
            }
            let raw = read_input(spec)?;
            let payload: ImportPayload =
                serde_json::from_str(&raw).context("解析 --payload JSON 失败")?;
            (
                payload.title,
                payload.dir,
                payload.text,
                payload.toml,
                payload.force || options.force,
                payload.subject,
                payload.grade,
                payload.term,
            )
        }
        None => {
            let title = options.title.context("缺少 --title（或使用 --payload）")?;
            let text_spec = options.text.context("缺少 --text（或使用 --payload）")?;
            if text_spec == "-" && options.toml.as_deref() == Some("-") {
                bail!("--text 与 --toml 不能同时从 stdin 读取，请改用 --payload -");
            }
            let text = read_input(&text_spec)?;
            let toml_text = match options.toml.as_deref() {
                Some(spec) => Some(read_input(spec)?),
                None => None,
            };
            let subject = options
                .subject
                .context("缺少 --subject（chinese|english）")?;
            (
                title,
                options.dir,
                text,
                toml_text,
                options.force,
                subject,
                options.grade,
                options.term,
            )
        }
    };

    lesson::import::import(
        &app.lessons_dir,
        lesson::import::ImportRequest {
            title: Some(title),
            text,
            toml: toml_text,
            dir: dir_arg,
            force,
            subject,
            grade,
            term,
        },
    )
}

fn read_input(spec: &str) -> Result<String> {
    if spec == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .context("读取 stdin 失败")?;
        Ok(buffer)
    } else {
        fs::read_to_string(spec).with_context(|| format!("读取文件失败: {spec}"))
    }
}

fn exit_with_json_error(err: &anyhow::Error) -> ! {
    eprintln!("{}", json!({ "error": format!("{err:#}") }));
    std::process::exit(1);
}

/// Resolve `--dir`/`--title` to `<lessons>/<dir>/lesson.txt`.
fn lesson_source_path(app: &AppConfig, title: Option<&str>, dir: Option<&str>) -> Result<PathBuf> {
    let dir_name = match dir {
        Some(dir) => {
            app_config::validate_dir_name(dir)?;
            dir.trim().to_string()
        }
        None => {
            let title = title.context("缺少 --title 或 --dir")?;
            app_config::safe_dir_name(title)
        }
    };
    let lesson_dir = app_config::resolve_lesson_dir(&app.lessons_dir, &dir_name)?;
    if !lesson_dir.is_dir() {
        bail!(
            "课时不存在：{}（先运行 dictation lesson import）",
            lesson_dir.display()
        );
    }
    let file = lesson_dir.join("lesson.txt");
    if !file.is_file() {
        bail!("课时缺少 lesson.txt：{}", file.display());
    }
    Ok(file)
}

fn cmd_lesson_plan(
    app: &AppConfig,
    title: Option<&str>,
    dir: Option<&str>,
    json: bool,
) -> Result<()> {
    let path = lesson_source_path(app, title, dir)?;
    cmd_plan(&path, json)
}

fn subject_label(subject: &str) -> &str {
    match subject {
        "chinese" => "语文",
        "english" => "英语",
        other => other,
    }
}

fn cmd_lesson_list(app: &AppConfig, json: bool) -> Result<()> {
    let lessons = lesson::list::scan(&app.lessons_dir)?;

    if json {
        let value = json!({
            "lessons": lessons.iter().map(lesson::list::json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else if lessons.is_empty() {
        println!("{} 下还没有已生成的课时", app.lessons_dir.display());
        println!("先运行：dictation --config <config> lesson import --payload -");
    } else {
        println!(
            "{:<24} {:<6} {:<8} {:>10}  标题",
            "目录", "科目", "词数", "时长"
        );
        for lesson in &lessons {
            println!(
                "{:<24} {:<6} {:<8} {:>10}  {}",
                truncate(&lesson.dir, 24),
                subject_label(&lesson.subject),
                lesson.item_count,
                format_clock(lesson.total_ms),
                lesson.title
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// result: read-only views over the SQLite session history
// ---------------------------------------------------------------------------

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn open_result_db(app: &AppConfig) -> Result<Db> {
    let path = app.db_path();
    if !path.is_file() {
        bail!(
            "还没有听写记录（数据库不存在：{}）；先运行 dictation serve",
            path.display()
        );
    }
    Db::open(&path)
}

fn cmd_result_list(app: &AppConfig, json_output: bool, limit: u32, all: bool) -> Result<()> {
    let db = open_result_db(app)?;
    let sessions = db.list_sessions(limit.max(1), all)?;

    if json_output {
        let payload = json!({
            "sessions": sessions.iter().map(session_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else if sessions.is_empty() {
        if all {
            println!("还没有任何听写记录");
        } else {
            println!("还没有已提交检查的听写记录（加 --all 可看不完整的）");
        }
    } else {
        println!(
            "{:<10} {:<8} {:<26} {:>7}  标题",
            "ID", "类型", "完成时间", "对/错"
        );
        for session in &sessions {
            let finished = session
                .finished_at
                .map(format_unix)
                .unwrap_or_else(|| "未提交检查".to_string());
            println!(
                "{:<10} {:<8} {:<26} {:>7}  {}",
                short_id(&session.id),
                session.kind,
                finished,
                format!("{}/{}", session.correct, session.wrong),
                session.title
            );
        }
    }
    Ok(())
}

fn load_session_result(db: &Db, session_id: Option<&str>, latest: bool) -> Result<SessionResult> {
    match (session_id.map(str::trim), latest) {
        (Some(_), true) => bail!("session_id 与 --latest 只能二选一"),
        (None, false) => bail!("需要提供 session_id，或使用 --latest"),
        (Some(id), false) => {
            if id.is_empty() {
                bail!("session_id 不能为空");
            }
            db.session_result(id)?
                .with_context(|| format!("session 不存在：{id}"))
        }
        (None, true) => {
            let session = db
                .latest_finished_session()?
                .context("还没有已提交检查的听写记录")?;
            db.session_result(&session.id)?.context("session 不存在")
        }
    }
}

fn print_session_header(result: &SessionResult) {
    let summary = &result.summary;
    println!("本次听写：{}（{}）", summary.title, summary.kind);
    println!("  session：{}", summary.id);
    println!(
        "  开始：{}  完成：{}",
        format_unix(summary.created_at),
        summary
            .finished_at
            .map(format_unix)
            .unwrap_or_else(|| "未提交检查".to_string())
    );
    println!(
        "  {} 项：{} 对 / {} 错",
        summary.item_count, summary.correct, summary.wrong
    );
}

fn print_wrong_words(wrong_words: &[WrongWord]) {
    if wrong_words.is_empty() {
        println!("错词：没有。");
        return;
    }
    println!("错词（{}）：", wrong_words.len());
    for word in wrong_words {
        let origin = word.origin_lesson.as_deref().unwrap_or("-");
        let due = word
            .due_at
            .map(format_unix)
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  {:>3}  {:<8}  ← {:<10}  lapses={}  下次 {}",
            word.position, word.text, origin, word.lapses, due
        );
    }
}

fn cmd_result_show(
    app: &AppConfig,
    session_id: Option<&str>,
    latest: bool,
    json_output: bool,
) -> Result<()> {
    let db = open_result_db(app)?;
    let result = load_session_result(&db, session_id, latest)?;

    if json_output {
        let payload = json!({
            "session": session_json(&result.summary),
            "items": result.items.iter().map(item_json).collect::<Vec<_>>(),
            "wrong_words": result.wrong_words.iter().map(wrong_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    print_session_header(&result);
    println!();
    for item in &result.items {
        let mark = match item.grade.as_deref() {
            Some("good") => "✓",
            Some("again") => "✗",
            _ => "·",
        };
        let origin = item.origin_lesson.as_deref().unwrap_or("-");
        let review = if item.review { "复习 " } else { "" };
        println!(
            "  {}  {:>3}  {:<10}  [{}{}]",
            mark, item.position, item.text, review, origin
        );
    }
    println!();
    print_wrong_words(&result.wrong_words);
    Ok(())
}

fn cmd_result_wrong(
    app: &AppConfig,
    session_id: Option<&str>,
    latest: bool,
    json_output: bool,
) -> Result<()> {
    let db = open_result_db(app)?;
    let result = load_session_result(&db, session_id, latest)?;
    if result.summary.finished_at.is_none() {
        bail!("本次听写还没提交检查");
    }

    if json_output {
        let payload = json!({
            "session": session_json(&result.summary),
            "wrong_words": result.wrong_words.iter().map(wrong_json).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    println!(
        "本次听写：{}（{}）",
        result.summary.title, result.summary.kind
    );
    println!(
        "  session：{}  {} 项：{} 对 / {} 错",
        result.summary.id, result.summary.item_count, result.summary.correct, result.summary.wrong
    );
    print_wrong_words(&result.wrong_words);
    Ok(())
}

// ---------------------------------------------------------------------------
// clean: cache cleanup, dry-run by default
// ---------------------------------------------------------------------------

fn parse_age(raw: &str) -> Result<std::time::Duration> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("--older-than 不能为空（示例：30d / 12h / 45m / 30）");
    }
    let lower = raw.to_ascii_lowercase();
    let (number, unit_secs) = if let Some(value) = lower.strip_suffix('d') {
        (value, 86_400.0)
    } else if let Some(value) = lower.strip_suffix('h') {
        (value, 3_600.0)
    } else if let Some(value) = lower.strip_suffix('m') {
        (value, 60.0)
    } else {
        (lower.as_str(), 86_400.0)
    };
    let value: f64 = number
        .trim()
        .parse()
        .with_context(|| format!("无法解析 --older-than：{raw}（示例：30d / 12h / 45m / 30）"))?;
    if !value.is_finite() || value < 0.0 {
        bail!("--older-than 必须是非负数：{raw}");
    }
    let seconds = value * unit_secs;
    if seconds > u64::MAX as f64 {
        bail!("--older-than 太大：{raw}");
    }
    Ok(std::time::Duration::from_secs_f64(seconds))
}

/// `DICTATION_CACHE_DIR` wins over the config, matching `gen`.
fn effective_cache_dir(app: &AppConfig) -> PathBuf {
    match std::env::var("DICTATION_CACHE_DIR") {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw.trim()),
        _ => app.cache_dir.clone(),
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn cmd_clean(
    app: &AppConfig,
    unused: bool,
    older_than: Option<&str>,
    apply: bool,
    json_output: bool,
) -> Result<()> {
    if !unused && older_than.is_none() {
        bail!("至少需要 --unused 或 --older-than 之一（示例：clean --unused --older-than 30d）");
    }
    let age = older_than.map(parse_age).transpose()?;
    let cache_dir = effective_cache_dir(app);

    let plan = cache::plan_cleanup(
        &cache_dir,
        &app.lessons_dir,
        &cache::CleanupOptions {
            unused,
            older_than: age,
        },
    );
    let (deleted, apply_errors) = if apply {
        cache::apply_cleanup(&cache_dir, &plan.files)
    } else {
        (0, Vec::new())
    };
    let mut errors = plan.errors.clone();
    errors.extend(apply_errors);

    let bytes = plan.bytes();
    if json_output {
        const FILES_CAP: usize = 500;
        let truncated = plan.files.len() > FILES_CAP;
        let mut payload = json!({
            "cache_dir": cache_dir.display().to_string(),
            "lessons_dir": app.lessons_dir.display().to_string(),
            "dry_run": !apply,
            "unused": unused,
            "older_than": older_than,
            "scanned": plan.scanned,
            "referenced": plan.referenced,
            "candidates": plan.files.len(),
            "bytes": bytes,
            "deleted": deleted,
            "files": plan
                .files
                .iter()
                .take(FILES_CAP)
                .map(|file| json!({ "path": file.name, "bytes": file.bytes }))
                .collect::<Vec<_>>(),
            "errors": errors,
        });
        if truncated {
            payload["truncated"] = json!(true);
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("缓存目录：{}", cache_dir.display());
        println!("课时目录：{}", app.lessons_dir.display());
        println!(
            "扫描 {} 个文件，引用 {} 个缓存条目，可清理 {} 个文件（{}）",
            plan.scanned,
            plan.referenced,
            plan.files.len(),
            human_bytes(bytes)
        );
        for file in plan.files.iter().take(50) {
            println!("  {}  {}", file.name, human_bytes(file.bytes));
        }
        if plan.files.len() > 50 {
            println!("  … 还有 {} 个文件未显示", plan.files.len() - 50);
        }
        if apply {
            println!("已删除 {deleted} 个文件");
        } else {
            println!("dry-run：本次没有删除任何文件，加上 --apply 才会真正删除");
        }
        for error in &errors {
            eprintln!("警告：{error}");
        }
    }

    if !errors.is_empty() {
        // Partial failures are reported in `errors`; still exit non-zero.
        std::process::exit(1);
    }
    Ok(())
}

fn count_clips(dir: &Path) -> usize {
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
                })
                .count()
        })
        .unwrap_or(0)
}

fn lang_label(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "中",
        Lang::En => "英",
        Lang::Mixed => "混",
    }
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn format_seconds(ms: u32) -> String {
    format!("{:.1}s", f64::from(ms) / 1_000.0)
}

fn format_clock(ms: u32) -> String {
    let total_seconds = ms / 1_000;
    format!("{:02}:{:02}", total_seconds / 60, total_seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_config(root: &Path) -> AppConfig {
        let config_path = root.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "[paths]\nbase = \"{}\"\nlessons = \"lessons\"\ndata = \"data\"\ncache = \"cache\"\nweb = \"web\"\n",
                root.display()
            ),
        )
        .unwrap();
        AppConfig::load(Some(config_path)).unwrap()
    }

    fn payload(root: &Path) -> PathBuf {
        root.join("payload.json")
    }

    fn write_payload(root: &Path, body: &str) -> PathBuf {
        let path = payload(root);
        fs::write(&path, body).unwrap();
        path
    }

    fn import(
        app: &AppConfig,
        payload_path: &Path,
        force: bool,
    ) -> Result<lesson::import::ImportOutcome> {
        import_lesson(
            app,
            ImportOptions {
                payload: Some(payload_path.to_string_lossy().to_string()),
                force,
                ..Default::default()
            },
        )
    }

    #[test]
    fn imports_lesson_and_toml_under_lessons_dir() {
        let temp = tempfile::tempdir().unwrap();
        let app = app_config(temp.path());
        let payload = write_payload(
            temp.path(),
            r#"{"title":"第5课","subject":"chinese","text":"摇晃\n粗壮\n","toml":"rate = 0.9\ngap = \"2s\"\n"}"#,
        );

        let outcome = import(&app, &payload, false).unwrap();
        assert!(outcome.created);
        assert_eq!(outcome.title, "第5课");
        assert_eq!(outcome.item_count, 2);
        assert!(outcome.total_ms > 0);
        assert_eq!(
            outcome.dir_path,
            app.lessons_dir.join("第5课").canonicalize().unwrap()
        );

        let lesson_text = fs::read_to_string(outcome.dir_path.join("lesson.txt")).unwrap();
        assert!(
            lesson_text.starts_with("# 第5课\n\n摇晃\n粗壮\n"),
            "title is persisted as a heading: {lesson_text:?}"
        );
        let toml_text = fs::read_to_string(outcome.dir_path.join("lesson.toml")).unwrap();
        assert!(toml_text.contains("rate = 0.9"));
        let parsed = lesson::parse_file(&outcome.dir_path.join("lesson.txt")).unwrap();
        assert_eq!(parsed.title, "第5课");
        assert_eq!(parsed.config.gap_ms, 2_000);
    }

    #[test]
    fn duplicate_import_requires_force() {
        let temp = tempfile::tempdir().unwrap();
        let app = app_config(temp.path());
        let payload = write_payload(
            temp.path(),
            r#"{"title":"第5课","subject":"chinese","text":"摇晃\n"}"#,
        );
        import(&app, &payload, false).unwrap();

        let err = import(&app, &payload, false).unwrap_err();
        assert!(format!("{err:#}").contains("--force"));

        let outcome = import(&app, &payload, true).unwrap();
        assert!(!outcome.created);
    }

    #[test]
    fn rejects_traversal_and_absolute_dirs() {
        let temp = tempfile::tempdir().unwrap();
        let app = app_config(temp.path());
        let payload = write_payload(
            temp.path(),
            r#"{"title":"第5课","subject":"chinese","dir":"../evil","text":"摇晃\n"}"#,
        );
        let err = import(&app, &payload, false).unwrap_err();
        assert!(format!("{err:#}").contains(".."));
        assert!(!temp.path().join("evil").exists());

        let payload = write_payload(
            temp.path(),
            r#"{"title":"第5课","subject":"chinese","dir":"/tmp/evil","text":"摇晃\n"}"#,
        );
        assert!(import(&app, &payload, false).is_err());
    }

    #[test]
    fn invalid_toml_leaves_no_files() {
        let temp = tempfile::tempdir().unwrap();
        let app = app_config(temp.path());
        let payload = write_payload(
            temp.path(),
            r#"{"title":"第5课","subject":"chinese","text":"摇晃\n","toml":"rate = 9.9\n"}"#,
        );
        assert!(import(&app, &payload, false).is_err());
        assert!(
            !app.lessons_dir.join("第5课").exists() || {
                // Directory may exist from an earlier failure, but no files.
                !app.lessons_dir.join("第5课/lesson.txt").exists()
            }
        );
    }

    #[test]
    fn title_generates_safe_dir_when_missing() {
        let temp = tempfile::tempdir().unwrap();
        let app = app_config(temp.path());
        let payload = write_payload(
            temp.path(),
            r#"{"title":"第6课/上","subject":"chinese","text":"窗户\n操场\n"}"#,
        );
        let outcome = import(&app, &payload, false).unwrap();
        assert!(outcome.dir_path.ends_with("第6课_上"));
        assert!(outcome.dir_path.starts_with(&app.lessons_dir));
    }

    #[test]
    fn flags_mode_requires_title_and_text() {
        let temp = tempfile::tempdir().unwrap();
        let app = app_config(temp.path());
        let err = import_lesson(&app, ImportOptions::default()).unwrap_err();
        assert!(format!("{err:#}").contains("--title"));

        let text_path = temp.path().join("words.txt");
        fs::write(&text_path, "旅行\n准备\n").unwrap();
        let err = import_lesson(
            &app,
            ImportOptions {
                title: Some("第7课".to_string()),
                text: Some(text_path.to_string_lossy().to_string()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("--subject"),
            "flags 模式必须要求 --subject: {err:#}"
        );

        let outcome = import_lesson(
            &app,
            ImportOptions {
                title: Some("第7课".to_string()),
                text: Some(text_path.to_string_lossy().to_string()),
                subject: Some("chinese".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(outcome.item_count, 2);
        assert_eq!(outcome.subject, "chinese");
    }

    #[test]
    fn payload_conflicts_with_flags() {
        let temp = tempfile::tempdir().unwrap();
        let app = app_config(temp.path());
        let payload = write_payload(
            temp.path(),
            r#"{"title":"第5课","subject":"chinese","text":"摇晃\n"}"#,
        );
        let err = import_lesson(
            &app,
            ImportOptions {
                payload: Some(payload.to_string_lossy().to_string()),
                title: Some("别的".to_string()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("--payload"));
    }

    #[test]
    fn parses_clean_ages() {
        assert_eq!(
            parse_age("30d").unwrap(),
            std::time::Duration::from_secs(30 * 86_400)
        );
        assert_eq!(
            parse_age("12h").unwrap(),
            std::time::Duration::from_secs(12 * 3_600)
        );
        assert_eq!(
            parse_age("45m").unwrap(),
            std::time::Duration::from_secs(45 * 60)
        );
        assert_eq!(
            parse_age("30").unwrap(),
            std::time::Duration::from_secs(30 * 86_400),
            "纯数字按天"
        );
        assert_eq!(parse_age("0").unwrap(), std::time::Duration::ZERO);
        assert_eq!(
            parse_age("1.5d").unwrap(),
            std::time::Duration::from_secs(129_600)
        );
        assert!(parse_age("").is_err());
        assert!(parse_age("abc").is_err());
        assert!(parse_age("-1d").is_err());
        assert!(parse_age("1w").is_err());
    }

    #[test]
    fn formats_human_bytes() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1_024), "1.0 KB");
        assert_eq!(human_bytes(1_234_567), "1.2 MB");
    }
}
