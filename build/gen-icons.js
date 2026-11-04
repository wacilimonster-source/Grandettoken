// 从同一套几何生成 Windows、标题栏/托盘及 Android 图标。
//
// 几何是唯一母版:G 表盘(圆环右上 56° 开口)+ 横针位置的三枚金币。
// `app/branding/logo-source.png` 是这套几何的 1024px 预览,方便在文件管理器里查看;
// 真正的尺寸/裁切规则全在这个脚本里。只依赖 Node 内置模块。
const zlib = require("zlib");
const fs = require("fs");
const path = require("path");

const root = path.join(__dirname, "..");
const masterDir = path.join(root, "app", "branding");
const winDir = path.join(root, "app", "src-tauri", "icons");
const webDir = path.join(root, "app", "src", "brand");
const androidDir = path.join(root, "android", "app", "src", "main", "res", "drawable-nodpi");
const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

// ── 几何(100×100 坐标) ─────────────────────────────────────────────
const TILE_RX = 23;          // 圆角半径(%)
const RING_R = 27;           // 圆环中心线半径
const STROKE = 10;           // 圆环笔画
const GAP_START = -80;       // 开口下沿(度,0=右,顺时针)
const GAP_END = -24;         // 开口上沿
const COINS = [              // 横针 = 三枚叠起的金币,自下而上收窄
  { x: 59, y: 52.2, w: 18, h: 3.6 },
  { x: 60, y: 48.2, w: 16, h: 3.6 },
  { x: 61, y: 44.2, w: 14, h: 3.6 },
];
const LIGHT = { ring: [0xee, 0xf3, 0xff], coin: [0xf2, 0xc0, 0x63] };
const INK = { ring: [0x1a, 0x21, 0x30], coin: [0xb0, 0x74, 0x1a] };
const MONO = { ring: [255, 255, 255], coin: [255, 255, 255] };

const clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);
const mix = (a, b, t) => [
  Math.round(a[0] + (b[0] - a[0]) * t),
  Math.round(a[1] + (b[1] - a[1]) * t),
  Math.round(a[2] + (b[2] - a[2]) * t),
];

function insideTile(x, y, inset, radius) {
  const lo = inset, hi = 100 - inset;
  const cx = clamp(x, lo + radius, hi - radius);
  const cy = clamp(y, lo + radius, hi - radius);
  return Math.hypot(x - cx, y - cy) <= radius;
}

// 右上开口:角度落在开口扇区内的环带被挖掉,再用两个圆头盖补回端点。
function inRing(x, y) {
  const dx = x - 50, dy = y - 50;
  const d = Math.hypot(dx, dy);
  if (Math.abs(d - RING_R) <= STROKE / 2) {
    const a = (Math.atan2(dy, dx) * 180) / Math.PI;
    if (!(a >= GAP_START && a <= GAP_END)) return true;
  }
  for (const deg of [GAP_START, GAP_END]) {
    const rad = (deg * Math.PI) / 180;
    const cx = 50 + RING_R * Math.cos(rad);
    const cy = 50 + RING_R * Math.sin(rad);
    if (Math.hypot(x - cx, y - cy) <= STROKE / 2) return true;
  }
  return false;
}

function inCapsule(x, y, r) {
  const rad = r.h / 2;
  const cy = r.y + rad;
  const cx = clamp(x, r.x + rad, r.x + r.w - rad);
  return Math.hypot(x - cx, y - cy) <= rad;
}

// 金币先判:横针压在圆环上时,金色盖住白色。
function markAt(x, y, opt) {
  const pivot = opt.markPivot || 0;
  const scale = opt.markScale || 1;
  const tx = opt.markTx || 0;
  const ty = opt.markTy || 0;
  const mx = pivot + (x - pivot - tx) / scale;
  const my = pivot + (y - pivot - ty) / scale;
  for (const coin of COINS) if (inCapsule(mx, my, coin)) return opt.colors.coin;
  if (inRing(mx, my)) return opt.colors.ring;
  return null;
}

const SAMPLE = [0, 0, 0, 0];
function tileColor(x, y, rim) {
  const t = clamp((x + y) / 200, 0, 1);
  let rgb = t < 0.55
    ? mix([0x1b, 0x23, 0x37], [0x12, 0x17, 0x22], t / 0.55)
    : mix([0x12, 0x17, 0x22], [0x0c, 0x0f, 0x16], (t - 0.55) / 0.45);
  const glow = clamp(1 - Math.hypot(x - 50, y - 42) / 62, 0, 1) * 0.2;
  rgb = mix(rgb, [0x5b, 0x8c, 0xff], glow);
  if (rim && insideTile(x, y, 0.6, 22.4) && !insideTile(x, y, 1.8, 21.2)) {
    rgb = mix(rgb, [255, 255, 255], 0.09);
  }
  return rgb;
}

function sample(x, y, opt) {
  if (opt.tile) {
    if (opt.rounded && !insideTile(x, y, 0, TILE_RX)) return (SAMPLE[3] = 0), SAMPLE;
    const mark = opt.mark ? markAt(x, y, opt) : null;
    if (mark) {
      SAMPLE[0] = mark[0]; SAMPLE[1] = mark[1]; SAMPLE[2] = mark[2]; SAMPLE[3] = 1;
      return SAMPLE;
    }
    const rgb = tileColor(x, y, opt.rim !== false);
    SAMPLE[0] = rgb[0]; SAMPLE[1] = rgb[1]; SAMPLE[2] = rgb[2]; SAMPLE[3] = 1;
    return SAMPLE;
  }
  if (opt.mark) {
    const mark = markAt(x, y, opt);
    if (!mark) return (SAMPLE[3] = 0), SAMPLE;
    SAMPLE[0] = mark[0]; SAMPLE[1] = mark[1]; SAMPLE[2] = mark[2]; SAMPLE[3] = 1;
    return SAMPLE;
  }
  return (SAMPLE[3] = 0), SAMPLE;
}

function render(size, opt, crop) {
  const area = crop || { x: 0, y: 0, w: 100, h: 100 };
  const n = size <= 48 ? 6 : size <= 160 ? 4 : size <= 512 ? 3 : 2;
  const out = Buffer.alloc(size * size * 4);
  for (let py = 0; py < size; py++) {
    for (let px = 0; px < size; px++) {
      let ar = 0, ag = 0, ab = 0, aa = 0;
      for (let sy = 0; sy < n; sy++) {
        for (let sx = 0; sx < n; sx++) {
          const x = area.x + ((px + (sx + 0.5) / n) * area.w) / size;
          const y = area.y + ((py + (sy + 0.5) / n) * area.h) / size;
          const s = sample(x, y, opt);
          ar += s[0] * s[3]; ag += s[1] * s[3]; ab += s[2] * s[3]; aa += s[3];
        }
      }
      const i = (py * size + px) * 4;
      if (aa > 0) {
        out[i] = Math.round(ar / aa);
        out[i + 1] = Math.round(ag / aa);
        out[i + 2] = Math.round(ab / aa);
      }
      out[i + 3] = Math.round((255 * aa) / (n * n));
    }
  }
  return out;
}

// ── PNG / ICO ────────────────────────────────────────────────────────
function crc32(buf) {
  let crc = 0xffffffff;
  for (const byte of buf) {
    let c = (crc ^ byte) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4), crc = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

function png(size, pixels) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    pixels.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([signature, chunk("IHDR", ihdr), chunk("IDAT", zlib.deflateSync(raw, { level: 9 })), chunk("IEND", Buffer.alloc(0))]);
}

function ico(entries) {
  const header = Buffer.alloc(6), directory = Buffer.alloc(entries.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(entries.length, 4);
  let offset = header.length + directory.length;
  for (const [i, entry] of entries.entries()) {
    const pos = i * 16;
    directory[pos] = entry.size === 256 ? 0 : entry.size;
    directory[pos + 1] = directory[pos];
    directory.writeUInt16LE(1, pos + 4);
    directory.writeUInt16LE(32, pos + 6);
    directory.writeUInt32LE(entry.data.length, pos + 8);
    directory.writeUInt32LE(offset, pos + 12);
    offset += entry.data.length;
  }
  return Buffer.concat([header, directory, ...entries.map((entry) => entry.data)]);
}

// ── 输出 ────────────────────────────────────────────────────────────
const tile = { tile: true, rounded: true, mark: true, rim: true, colors: LIGHT };
const markCrop = { x: 15, y: 15, w: 70, h: 70 };
const icons = new Map();

fs.mkdirSync(masterDir, { recursive: true });
fs.mkdirSync(winDir, { recursive: true });
fs.mkdirSync(webDir, { recursive: true });
fs.mkdirSync(androidDir, { recursive: true });

for (const size of [16, 24, 32, 48, 64, 128, 256]) {
  icons.set(size, png(size, render(size, tile)));
}

fs.writeFileSync(path.join(masterDir, "logo-source.png"), png(1024, render(1024, tile)));
fs.writeFileSync(path.join(winDir, "32x32.png"), icons.get(32));
fs.writeFileSync(path.join(winDir, "128x128.png"), icons.get(128));
fs.writeFileSync(path.join(winDir, "icon.png"), icons.get(256));
fs.writeFileSync(path.join(winDir, "icon.ico"), ico([16, 24, 32, 48, 64, 128, 256].map((size) => ({ size, data: icons.get(size) }))));

fs.writeFileSync(path.join(webDir, "logo.png"), icons.get(128));
fs.writeFileSync(path.join(webDir, "mark.png"), png(128, render(128, { tile: false, mark: true, colors: LIGHT }, markCrop)));
fs.writeFileSync(path.join(webDir, "mark-ink.png"), png(128, render(128, { tile: false, mark: true, colors: INK }, markCrop)));
fs.writeFileSync(path.join(webDir, "logo-tray.png"), png(128, render(128, { ...tile, markScale: 0.78, markPivot: 0, markTy: -1 })));

// Android 自适应图标:背景铺满、前景 G 占画布 60%(66dp 安全区内),系统只负责裁切。
fs.writeFileSync(path.join(androidDir, "ic_launcher_background.png"), png(432, render(432, { tile: true, rounded: false, mark: false, rim: false })));
fs.writeFileSync(path.join(androidDir, "ic_launcher_foreground.png"), png(432, render(432, { tile: false, mark: true, colors: LIGHT, markScale: 0.9375, markPivot: 50 })));
fs.writeFileSync(path.join(androidDir, "ic_launcher_monochrome.png"), png(432, render(432, { tile: false, mark: true, colors: MONO, markScale: 0.9375, markPivot: 50 })));

console.log("品牌图标已生成:");
console.log("  母版  app/branding/logo-source.png (1024)");
console.log("  Windows app/src-tauri/icons/{32,128,256}.png + icon.ico (16–256)");
console.log("  前端  app/src/brand/{logo,mark,mark-ink,logo-tray}.png (128)");
console.log("  Android drawable-nodpi/ic_launcher_{background,foreground,monochrome}.png (432)");
