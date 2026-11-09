// 用真实 Chromium 画布检查托盘数字及 IPC 帧。开发依赖 playwright-core + 系统 Edge。
// 用法:NODE_PATH=<playwright-core 所在 node_modules> node build/verify-tray.js [预览图路径]
const fs = require("fs");
const path = require("path");
const os = require("os");
const assert = require("assert");
const { chromium } = require("playwright-core");

const app = fs.readFileSync(path.join(__dirname, "../app/src/app.js"), "utf8");
const start = app.indexOf("/** 托盘缩写:");
const end = app.indexOf("/** 设置页「渠道」页签:", start);
assert(start >= 0 && end > start, "找不到托盘实现");
const implementation = app.slice(start, end);
const preview = path.resolve(process.argv[2] || path.join(os.tmpdir(), "grandettoken-tray-preview.png"));

(async () => {
  const browser = await chromium.launch({
    executablePath: process.env.EDGE_BIN || path.join(process.env["ProgramFiles(x86)"], "Microsoft/Edge/Application/msedge.exe"),
    headless: true,
  });
  try {
    const page = await browser.newPage({ viewport: { width: 680, height: 760 }, deviceScaleFactor: 1 });
    await page.setContent('<!doctype html><meta charset="utf-8"><body></body>');
    await page.addScriptTag({ content: `(() => {
      const TONE_HEX = { ok: "#8bc9a2", warn: "#edc185", bad: "#ef9d94", off: "#3a4356" };
      const CFG = { trayShowNumber: true };
      const channel = { name: "测试渠道", hasKey: true, valid: true, kind: "amount", remaining: 74 };
      const PILL = { list: [channel], sorted: [channel], idx: 0 };
      const control = { size: 16, text: "¥74.00", frames: [], block: false, release: null, fail: false };
      const $ = () => ({ textContent: control.text });
      const toneOf = c => c.tone || "ok";
      const remainRatio = c => c.ratio ?? null;
      const setInterval = () => 0;
      class Image { constructor() { this.complete = false; this.naturalWidth = 0; } }
      const invoke = (command, args) => {
        if (command === "tray_icon_size") return Promise.resolve(control.size);
        if (command !== "set_tray_icon") throw new Error("意外的命令: " + command);
        control.frames.push(args);
        if (control.fail) { control.fail = false; return Promise.reject(new Error("模拟失败")); }
        if (control.block) return new Promise(resolve => { control.release = resolve; });
        return Promise.resolve();
      };
      ${implementation}
      window.trayTest = { trayAbbr, trayNumber, drawTrayNumber, drawTrayIcon, refreshTraySize, CFG, channel, control };
    })();` });

    const result = await page.evaluate(async () => {
      const h = window.trayTest;
      const checks = [];
      const check = (name, ok) => checks.push({ name, ok: !!ok });
      const tick = () => new Promise(resolve => setTimeout(resolve, 0));
      for (const [value, expected] of [
        [0, "0"], [0.03, "0.0"], [0.4, "0.4"], [5, "5"], [-0.4, "-0.4"],
        [74.2, "74"], [999, "999"], [1000, "1k"], [1200, "1.2k"],
        [10000, "1w"], [112000, "11w"], [999999, "100w"],
      ]) check(`缩写 ${value} → ${expected}`, h.trayAbbr(value) === expected);
      check("无效余额不画成零", h.trayNumber({ hasKey: true, valid: true, kind: "amount", remaining: null }) === "");
      check("非有限数值不画为数字", h.trayNumber({ hasKey: true, valid: true, kind: "amount", remaining: NaN }) === "");
      check("失败且无旧值回退品牌", h.trayNumber({ hasKey: true, valid: false, kind: "amount", remaining: 123 }) === "");
      check("失联但有旧值继续显示", h.trayNumber({ hasKey: true, valid: false, stale: true, kind: "amount", remaining: 123 }) === "123");
      check("百分比零值正确显示", h.trayNumber({ hasKey: true, valid: true, kind: "percent", ratio: 0 }) === "0");
      check("无百分比不画成零", h.trayNumber({ hasKey: true, valid: true, kind: "percent" }) === "");

      for (const size of [16, 20, 24, 32, 40]) {
        const digitImages = new Set();
        for (const value of ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "74", "100", "0.4", "1.2k", "10w", "-0.4"]) {
          const canvas = document.createElement("canvas");
          canvas.width = canvas.height = size;
          const g = canvas.getContext("2d");
          h.drawTrayNumber(g, size, value, "ok");
          const data = g.getImageData(0, 0, size, size).data;
          let left = size, right = -1, top = size, bottom = -1;
          for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
            if (data[(y * size + x) * 4 + 3]) {
              left = Math.min(left, x); right = Math.max(right, x);
              top = Math.min(top, y); bottom = Math.max(bottom, y);
            }
          }
          check(`${size}px ${value} 有笔画且未触及边缘`, right >= left && left >= 1 && right < size - 1 && top >= 1 && bottom < size - 1);
          if (/^\d$/.test(value)) digitImages.add(canvas.toDataURL());
        }
        check(`${size}px 0–9 字形各不相同`, digitImages.size === 10);
      }
      const off = document.createElement("canvas"); off.width = off.height = 16;
      const offCtx = off.getContext("2d"); h.drawTrayNumber(offCtx, 16, "74", "off");
      const offPixels = offCtx.getImageData(0, 0, 16, 16).data;
      check("深色失联底使用浅色数字", Array.from(offPixels).some((v, i) => i % 4 === 3 && v && offPixels[i - 1] === 255));

      await tick();
      h.drawTrayIcon(); await tick();
      let last = h.control.frames.at(-1);
      check("100% 按 16×16 发送并保留完整 tooltip", last.size === 16 && atob(last.rgbaB64).length === 16 * 16 * 4 && last.tooltip === "测试渠道 ¥74.00");
      const count = h.control.frames.length;
      h.drawTrayIcon(); await tick();
      check("相同画面不重复发送", h.control.frames.length === count);
      h.control.size = 20; await h.refreshTraySize(); await tick();
      last = h.control.frames.at(-1);
      check("缩放变化重画 20×20", last.size === 20 && atob(last.rgbaB64).length === 20 * 20 * 4);
      h.CFG.trayShowNumber = false; h.drawTrayIcon(); await tick();
      check("关闭数字后仍能发送品牌图标", h.control.frames.at(-1).size === 20 && h.control.frames.at(-1).rgbaB64 !== last.rgbaB64);
      h.CFG.trayShowNumber = true;

      h.control.block = true; h.control.text = "74"; h.channel.remaining = 74; h.drawTrayIcon();
      h.control.text = "75"; h.channel.remaining = 75; h.drawTrayIcon();
      h.control.size = 24; await h.refreshTraySize();
      h.control.text = "76"; h.channel.remaining = 76; h.drawTrayIcon();
      const pending = h.control.frames.length;
      h.control.block = false; h.control.release(); await tick();
      last = h.control.frames.at(-1);
      check("并发变化合并为最新帧", h.control.frames.length === pending + 1 && last.size === 24 && last.tooltip === "测试渠道 76");
      h.control.fail = true; h.control.text = "77"; h.channel.remaining = 77; h.drawTrayIcon(); await tick();
      const afterFail = h.control.frames.length;
      h.drawTrayIcon(); await tick();
      check("发送失败后同一帧可重试", h.control.frames.length === afterFail + 1);

      document.body.innerHTML = `<style>
        body{font:14px 'Segoe UI',sans-serif;background:#f5f5f5;color:#20232a;margin:24px}
        table{border-collapse:collapse}th,td{padding:6px 9px;border:1px solid #c9cdd4;text-align:center}
        .sample{display:inline-flex;gap:9px;align-items:center;padding:5px;background:#1a1d24}
        .light{background:#e7e9ed}.zoom{width:80px;height:80px;image-rendering:pixelated}
        small{color:#515b69}
      </style><h3>托盘数字:真实像素与放大预览</h3><small>每格左侧是实际尺寸,右侧是逐像素放大。</small><table id="grid"></table>`;
      const grid = document.getElementById("grid");
      const head = grid.insertRow();
      for (const label of ["示例", "16px / 100%", "20px / 125%", "24px / 150%", "32px / 200%"]) {
        const th = document.createElement("th"); th.textContent = label; head.appendChild(th);
      }
      for (const [value, tone] of [["74", "ok"], ["100", "warn"], ["0.4", "bad"], ["1.2k", "ok"], ["10w", "warn"], ["-0.4", "off"]]) {
        const row = grid.insertRow(); row.insertCell().textContent = value;
        for (const size of [16, 20, 24, 32]) {
          const cell = row.insertCell();
          const box = document.createElement("div"); box.className = "sample" + (tone === "bad" ? " light" : "");
          const canvas = document.createElement("canvas"); canvas.width = canvas.height = size;
          const g = canvas.getContext("2d");
          g.beginPath(); g.roundRect(0, 0, size, size, size * 0.22);
          g.fillStyle = { ok: "#8bc9a2", warn: "#edc185", bad: "#ef9d94", off: "#3a4356" }[tone]; g.fill();
          h.drawTrayNumber(g, size, value, tone);
          const zoom = document.createElement("canvas"); zoom.width = zoom.height = size; zoom.className = "zoom";
          zoom.getContext("2d").drawImage(canvas, 0, 0);
          box.append(canvas, zoom); cell.appendChild(box);
        }
      }
      return checks;
    });
    await page.screenshot({ path: preview, fullPage: true });
    const failed = result.filter(x => !x.ok);
    console.log(`${result.length - failed.length}/${result.length} 项通过; 预览: ${preview}`);
    for (const item of failed) console.error("失败:", item.name);
    if (failed.length) process.exitCode = 1;
  } finally {
    await browser.close();
  }
})().catch(e => { console.error(e); process.exitCode = 1; });
