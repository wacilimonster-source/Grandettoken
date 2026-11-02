// 从同一张母版生成 Windows、标题栏/托盘及 Android 启动图标。
// 只依赖 Node 内置模块;母版须为非交错的 8 位 RGB/RGBA 正方形 PNG。
const zlib = require("zlib");
const fs = require("fs");
const path = require("path");

const root = path.join(__dirname, "..");
const source = path.join(root, "app", "branding", "logo-source.png");
const winDir = path.join(root, "app", "src-tauri", "icons");
const webDir = path.join(root, "app", "src", "brand");
const androidDir = path.join(root, "android", "app", "src", "main", "res", "drawable-nodpi");
const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

function paeth(a, b, c) {
  const p = a + b - c;
  const da = Math.abs(p - a), db = Math.abs(p - b), dc = Math.abs(p - c);
  return da <= db && da <= dc ? a : db <= dc ? b : c;
}

function decodePng(file) {
  const bytes = fs.readFileSync(file);
  if (!bytes.subarray(0, 8).equals(signature)) throw new Error("图标母版不是 PNG");
  let width, height, channels, depth, interlace;
  const chunks = [];
  for (let pos = 8; pos + 12 <= bytes.length;) {
    const len = bytes.readUInt32BE(pos);
    const type = bytes.toString("ascii", pos + 4, pos + 8);
    const data = bytes.subarray(pos + 8, pos + 8 + len);
    if (data.length !== len) throw new Error("PNG 数据不完整");
    if (type === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      depth = data[8];
      channels = data[9] === 2 ? 3 : data[9] === 6 ? 4 : 0;
      interlace = data[12];
    } else if (type === "IDAT") chunks.push(data);
    else if (type === "IEND") break;
    pos += len + 12;
  }
  if (!width || width !== height || depth !== 8 || !channels || interlace !== 0) {
    throw new Error("图标母版须为非交错的 8 位 RGB/RGBA 正方形 PNG");
  }
  const raw = zlib.inflateSync(Buffer.concat(chunks));
  const stride = width * channels;
  if (raw.length !== (stride + 1) * height) throw new Error("PNG 扫描行长度不符");
  const pixels = Buffer.alloc(width * height * 4);
  let previous = Buffer.alloc(stride);
  let row = Buffer.alloc(stride);
  let offset = 0;
  for (let y = 0; y < height; y++) {
    const filter = raw[offset++];
    if (filter > 4) throw new Error(`不支持 PNG filter ${filter}`);
    for (let x = 0; x < stride; x++) {
      const left = x >= channels ? row[x - channels] : 0;
      const up = previous[x];
      const upperLeft = x >= channels ? previous[x - channels] : 0;
      const predictor = filter === 0 ? 0 : filter === 1 ? left : filter === 2 ? up
        : filter === 3 ? Math.floor((left + up) / 2) : paeth(left, up, upperLeft);
      row[x] = (raw[offset++] + predictor) & 255;
    }
    for (let x = 0; x < width; x++) {
      const from = x * channels, to = (y * width + x) * 4;
      pixels[to] = row[from];
      pixels[to + 1] = row[from + 1];
      pixels[to + 2] = row[from + 2];
      pixels[to + 3] = channels === 4 ? row[from + 3] : 255;
    }
    [previous, row] = [row, previous];
  }
  return { size: width, pixels };
}

// 按像素面积做缩小采样,并在 alpha 加权后计算颜色,避免透明边出现黑边。
function resize(image, size) {
  const scale = image.size / size;
  if (scale < 1) throw new Error(`图标母版小于 ${size}×${size}`);
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) {
    const y0 = y * scale, y1 = (y + 1) * scale;
    for (let x = 0; x < size; x++) {
      const x0 = x * scale, x1 = (x + 1) * scale;
      let r = 0, g = 0, b = 0, a = 0;
      for (let sy = Math.floor(y0); sy < Math.min(image.size, Math.ceil(y1)); sy++) {
        const wy = Math.min(y1, sy + 1) - Math.max(y0, sy);
        for (let sx = Math.floor(x0); sx < Math.min(image.size, Math.ceil(x1)); sx++) {
          const wx = Math.min(x1, sx + 1) - Math.max(x0, sx);
          const i = (sy * image.size + sx) * 4;
          const weight = wx * wy * image.pixels[i + 3] / 255;
          r += image.pixels[i] * weight;
          g += image.pixels[i + 1] * weight;
          b += image.pixels[i + 2] * weight;
          a += weight;
        }
      }
      const i = (y * size + x) * 4;
      if (a) {
        out[i] = Math.round(r / a);
        out[i + 1] = Math.round(g / a);
        out[i + 2] = Math.round(b / a);
        out[i + 3] = Math.round(255 * a / (scale * scale));
      }
    }
  }
  return out;
}

// Windows 及标题栏图标使用带抗锯齿的圆角;Android 的形状由系统裁切。
function rounded(pixels, size) {
  const out = Buffer.from(pixels);
  const radius = size * 0.2;
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const px = x + 0.5, py = y + 0.5;
    const cx = Math.min(Math.max(px, radius), size - radius);
    const cy = Math.min(Math.max(py, radius), size - radius);
    const coverage = Math.max(0, Math.min(1, radius + 0.5 - Math.hypot(px - cx, py - cy)));
    const alpha = (y * size + x) * 4 + 3;
    out[alpha] = Math.round(out[alpha] * coverage);
  }
  return out;
}

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

// 自适应图标 108dp 中的安全区为中心 66dp。将完整画面缩入安全区,
// 向外延伸边缘颜色填满背景,避免圆形/方圆形启动器裁掉主体。
function androidArt(image) {
  const size = 432, innerSize = 264, inset = (size - innerSize) / 2;
  const inner = resize(image, innerSize);
  const out = Buffer.alloc(size * size * 4);
  for (let y = 0; y < size; y++) for (let x = 0; x < size; x++) {
    const sx = Math.max(0, Math.min(innerSize - 1, x - inset));
    const sy = Math.max(0, Math.min(innerSize - 1, y - inset));
    inner.copy(out, (y * size + x) * 4, (sy * innerSize + sx) * 4, (sy * innerSize + sx + 1) * 4);
  }
  return png(size, out);
}

const image = decodePng(source);
for (const dir of [winDir, webDir, androidDir]) fs.mkdirSync(dir, { recursive: true });
const sizes = [16, 24, 32, 48, 64, 128, 256];
const icons = new Map(sizes.map((size) => [size, png(size, rounded(resize(image, size), size))]));
fs.writeFileSync(path.join(winDir, "32x32.png"), icons.get(32));
fs.writeFileSync(path.join(winDir, "128x128.png"), icons.get(128));
fs.writeFileSync(path.join(winDir, "icon.png"), icons.get(256));
fs.writeFileSync(path.join(winDir, "icon.ico"), ico(sizes.map((size) => ({ size, data: icons.get(size) }))));
fs.writeFileSync(path.join(webDir, "logo.png"), icons.get(128));
fs.writeFileSync(path.join(androidDir, "ic_launcher_art.png"), androidArt(image));
console.log("图标已从 app/branding/logo-source.png 生成到 Windows、标题栏/托盘及 Android 资源目录。");
