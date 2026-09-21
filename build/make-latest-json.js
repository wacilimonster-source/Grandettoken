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
const exePath = path.join(nsisDir, exe);
// 只信 .sig 存在不够:清单写的是 exe 的名字,exe 没打出来(或名字对不上)时
// 清单照样生成得出,发出去就是"检测到新版,下载 404"(报告 O-11)。
if (!fs.existsSync(exePath)) {
  console.error(`找不到安装包 ${exePath} —— 清单不能生成`);
  process.exit(1);
}
// 单 exe 交付的最后一道闸:同目录里若混进 WebView2Loader.dll,说明静态加载器
// 那步没生效,发出去的包在别的机器上会报「找不到 DLL」。
const strayDll = fs.readdirSync(nsisDir).filter((f) => /webview2loader\.dll$/i.test(f));
if (strayDll.length) {
  console.error(`bundle 目录里出现了 ${strayDll.join(', ')} —— 单 exe 交付被破坏,先跑 build/make-webview2-static.ps1`);
  process.exit(1);
}
const crypto = require('crypto');
const sha256 = crypto.createHash('sha256').update(fs.readFileSync(exePath)).digest('hex');

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
console.log(`sha256   = ${sha256}`);
console.log(`url      = ${json.platforms['windows-x86_64'].url}`);
console.log(`written  = ${out}`);
