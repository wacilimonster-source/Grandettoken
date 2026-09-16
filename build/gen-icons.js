// 生成应用图标:圆角方块 + 蓝紫渐变 + 内部深色方块,对应界面里的 .mark
// 不依赖任何图形库,直接手写 PNG/ICO 二进制
const zlib = require('zlib');
const fs = require('fs');
const path = require('path');

const OUT = path.join(__dirname, '..', 'app', 'src-tauri', 'icons');
fs.mkdirSync(OUT, { recursive: true });

const lerp = (a, b, t) => a + (b - a) * t;

function render(size) {
  const px = Buffer.alloc(size * size * 4);
  const S = size / 128; // 以 128 为设计基准
  const r = 26 * S;     // 圆角半径
  const inset = 47 * S; // 内部方块起点
  const inner = 34 * S; // 内部方块边长
  const innerR = 6 * S;

  // 圆角矩形覆盖率,带 1px 抗锯齿
  const cover = (x, y, w, h, rad) => {
    const cx = Math.min(Math.max(x, rad), w - rad);
    const cy = Math.min(Math.max(y, rad), h - rad);
    const d = Math.hypot(x - cx, y - cy);
    if (d <= rad - 0.5) return 1;
    if (d >= rad + 0.5) return 0;
    return rad + 0.5 - d;
  };

  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      const i = (y * size + x) * 4;
      const a = cover(x + 0.5, y + 0.5, size, size, r);
      if (a <= 0) continue;

      // 135° 线性渐变 #5b8cff -> #8b5bff
      const t = (x / size + y / size) / 2;
      let cr = lerp(0x5b, 0x8b, t);
      let cg = lerp(0x8c, 0x5b, t);
      let cb = lerp(0xff, 0xff, t);

      // 内部深色方块
      const ia = cover(x + 0.5 - inset, y + 0.5 - inset, inner, inner, innerR);
      if (ia > 0) {
        cr = lerp(cr, 0x12, ia);
        cg = lerp(cg, 0x14, ia);
        cb = lerp(cb, 0x1a, ia);
      }

      px[i] = Math.round(cr);
      px[i + 1] = Math.round(cg);
      px[i + 2] = Math.round(cb);
      px[i + 3] = Math.round(a * 255);
    }
  }
  return px;
}

function crc32(buf) {
  let c, crc = 0xffffffff;
  for (let n = 0; n < buf.length; n++) {
    c = (crc ^ buf[n]) & 0xff;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    crc = (crc >>> 8) ^ c;
  }
  return (crc ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

function png(size, px) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr[8] = 8;  // bit depth
  ihdr[9] = 6;  // RGBA
  // 每行前置 filter byte 0
  const raw = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    px.copy(raw, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', zlib.deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// ICO 容器,条目内直接嵌 PNG(Vista+ 支持)
function ico(entries) {
  const header = Buffer.alloc(6);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2); // type: icon
  header.writeUInt16LE(entries.length, 4);

  const dir = Buffer.alloc(16 * entries.length);
  let offset = 6 + dir.length;
  const blobs = [];

  entries.forEach((e, i) => {
    const o = i * 16;
    dir[o] = e.size >= 256 ? 0 : e.size;
    dir[o + 1] = e.size >= 256 ? 0 : e.size;
    dir[o + 2] = 0;
    dir[o + 3] = 0;
    dir.writeUInt16LE(1, o + 4);
    dir.writeUInt16LE(32, o + 6);
    dir.writeUInt32LE(e.png.length, o + 8);
    dir.writeUInt32LE(offset, o + 12);
    offset += e.png.length;
    blobs.push(e.png);
  });

  return Buffer.concat([header, dir, ...blobs]);
}

const sizes = [32, 128, 256];
const rendered = {};
for (const s of sizes) {
  const buf = png(s, render(s));
  rendered[s] = buf;
  if (s !== 256) fs.writeFileSync(path.join(OUT, `${s}x${s}.png`), buf);
}
fs.writeFileSync(path.join(OUT, 'icon.png'), rendered[256]);
fs.writeFileSync(path.join(OUT, 'icon.ico'), ico(sizes.map(s => ({ size: s, png: rendered[s] }))));

console.log('icons written to', OUT);
for (const f of fs.readdirSync(OUT)) {
  console.log(' ', f, fs.statSync(path.join(OUT, f)).size, 'bytes');
}
