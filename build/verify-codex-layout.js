// 用真实 app.js 输出和 styles.css 验证 Codex 收起行,与用户运行中的应用隔离。
// 开发期依赖:jsdom + playwright-core;用系统 Edge。用法:node build/verify-codex-layout.js [输出目录]
const fs = require("fs");
const path = require("path");
const os = require("os");
const { execFileSync } = require("child_process");
const { chromium } = require("playwright-core");
const root = path.resolve(__dirname, "..");
const out = path.resolve(process.argv[2] || path.join(os.tmpdir(), "Grandettoken-codex-research", "layout-0.1.74"));
fs.mkdirSync(out, { recursive: true });
const fixturePath = path.join(out, "fixtures.json");
execFileSync(process.execPath, [path.join(__dirname, "jsdom-smoke.js")], {
  cwd: root, env: { ...process.env, CODEX_LAYOUT_FIXTURES: fixturePath }, stdio: "pipe",
});
const fixtures = JSON.parse(fs.readFileSync(fixturePath, "utf8"));
const css = fs.readFileSync(path.join(root, "app/src/styles.css"), "utf8");
const icon = "data:image/svg+xml;base64," + fs.readFileSync(path.join(root, "app/src/logos/chatgpt.svg")).toString("base64");
(async () => {
  const browser = await chromium.launch({ executablePath: process.env.EDGE_BIN || path.join(process.env["ProgramFiles(x86)"], "Microsoft/Edge/Application/msedge.exe"), headless: true });
  const page = await browser.newPage();
  const results = [];
  try {
    for (const scale of [1, 1.15, 1.3]) for (const form of ["panel", "compact"]) for (const fixture of fixtures) {
      const width = Math.round(400 * scale), height = Math.round((form === "compact" ? 46 : 140) * scale);
      await page.setViewportSize({ width, height });
      await page.setContent('<!doctype html><meta charset="utf-8"><style>' + css + '</style><body class="form-' + form + '"><div class="list">' + fixture.markup.replaceAll("logos/chatgpt.svg", icon) + '</div></body>');
      await page.evaluate((s) => document.documentElement.style.setProperty("--u", String(s)), scale);
      const check = await page.evaluate(({ dual, form }) => {
        const row = document.querySelector(".row"), rr = row.getBoundingClientRect();
        const parts = [...row.querySelectorAll(".quota-label,.quota-number,.quota-secondary")];
        const overflow = parts.filter((el) => { const r = el.getBoundingClientRect(); return r.left < rr.left - 1 || r.right > rr.right + 1 || r.bottom > innerHeight + 1; });
        const secondary = row.querySelector(".quota-secondary");
        const visible = !dual || (secondary && secondary.getBoundingClientRect().height > 0 && getComputedStyle(secondary.parentElement).display !== "none");
        const mainSize = parseFloat(getComputedStyle(row.querySelector(".quota-number")).fontSize);
        const secondarySize = secondary ? parseFloat(getComputedStyle(secondary.querySelector("b")).fontSize) : 0;
        return { ok: overflow.length === 0 && visible && (form !== "panel" || !dual || mainSize >= secondarySize * 1.4),
          overflow: overflow.map((e) => e.textContent), visible, mainSize, secondarySize, rowHeight: rr.height };
      }, { dual: fixture.dual, form });
      results.push({ name: fixture.name, scale, form, ...check });
      if (scale === 1 && ["normal", "short-limited", "stale"].includes(fixture.name)) await page.screenshot({ path: path.join(out, fixture.name + "-" + form + ".png") });
    }
  } finally { await browser.close(); }
  fs.writeFileSync(path.join(out, "report.json"), JSON.stringify(results, null, 2));
  const failed = results.filter((r) => !r.ok);
  console.log(JSON.stringify({ passed: results.length - failed.length, total: results.length, failed, out }, null, 2));
  if (failed.length) process.exitCode = 1;
})().catch((e) => { console.error(e); process.exitCode = 1; });
