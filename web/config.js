// 运行模式配置，由 app.js 读取。
// - mode: "server"  本地 dictation serve（同源 /api 与 /media）
// - mode: "static"  Cloudflare Pages 壳 + R2 只读数据（发布时由
//   `dictation publish --shell` 生成 dist/shell/config.js 覆盖本文件）
// 缺失、解析失败或 mode 非 static 时，app.js 一律按 server 处理。
window.DICTATION_CONFIG = { mode: 'server' };
