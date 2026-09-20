// 生成更新清单 latest.json。
//
// 用法: node build/make-latest-json.js
// 输出: app/src-tauri/target/release/bundle/nsis/latest.json
//
// 这个文件要和应用一起上传到 GitHub Release,更新插件靠它知道"有没有新版、
// 从哪下载、签名是多少"。signature 是 .sig 文件的**内容**,不是路径。
// 平台键必须与插件认定的 OS-ARCH 一致(Windows x64 = windows-x86_64)。
const fs = require('fs');
const path = require('path');

const REPO = 'wacilimonster-source/Grandettoken';
const root = path.join(__dirname, '..');
const src = path.join(root, 'app', 'src-tauri');
const conf = JSON.parse(fs.readFileSync(path.join(src, 'tauri.conf.json'), 'utf8'));
const version = conf.version;
const nsisDir = path.join(src, 'target', 'release', 'bundle', 'nsis');

// 版本号两处必须一致:Cargo.toml 是 Rust 侧 env!("CARGO_PKG_VERSION") 的来源,
// tauri.conf.json 是安装包文件名与更新清单的来源。不一致会导致"装完检测到自己
// 是新版本"或者永远检测不到 —— 发版前先断言。
const cargo = fs.readFileSync(path.join(src, 'Cargo.toml'), 'utf8');
const cargoVersion = (cargo.match(/^version\s*=\s*"([^"]+)"/m) || [])[1];
if (cargoVersion !== version) {
  console.error(`版本号不一致:Cargo.toml=${cargoVersion}  tauri.conf.json=${version}`);
  process.exit(1);
}

const product = conf.productName;
const exe = `${product}_${version}_x64-setup.exe`;
const sigPath = path.join(nsisDir, `${exe}.sig`);
if (!fs.existsSync(sigPath)) {
  console.error(`找不到签名 ${sigPath}。构建时必须设置 TAURI_SIGNING_PRIVATE_KEY。`);
  process.exit(1);
}

const tag = `v${version}`;
const json = {
  version,
  notes: process.argv.slice(2).join(' ') || `Grandettoken ${version}`,
  pub_date: new Date().toISOString(),
  platforms: {
    'windows-x86_64': {
      signature: fs.readFileSync(sigPath, 'utf8').trim(),
      url: `https://github.com/${REPO}/releases/download/${tag}/${exe}`,
    },
  },
};

const out = path.join(nsisDir, 'latest.json');
fs.writeFileSync(out, JSON.stringify(json, null, 2) + '\n');
console.log(`version  = ${version}  (Cargo.toml 一致)`);
console.log(`installer= ${exe}`);
console.log(`url      = ${json.platforms['windows-x86_64'].url}`);
console.log(`written  = ${out}`);
