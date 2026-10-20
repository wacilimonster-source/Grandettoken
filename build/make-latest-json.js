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

const root = path.join(__dirname, '..');
const src = path.join(root, 'app', 'src-tauri');
const conf = JSON.parse(fs.readFileSync(path.join(src, 'tauri.conf.json'), 'utf8'));
const version = conf.version;
// 仓库名从更新端点里取,不再第三处硬编码:改名时 endpoint 与清单必须一起改,
// 分两处写迟早有一处忘(P3-22)。
const endpoint = ((conf.plugins || {}).updater || {}).endpoints?.[0] || '';
const repoFromEndpoint = (endpoint.match(/https:\/\/github\.com\/([^/]+\/[^/]+)\//) || [])[1];
if (!repoFromEndpoint) {
  console.error('无法从 tauri.conf.json 的 plugins.updater.endpoints 推出仓库名,检查端点写法:', endpoint);
  process.exit(1);
}
const REPO = repoFromEndpoint;
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

// **第三处:Cargo.lock。** 升版本号时最容易漏的就是它 —— 改了 Cargo.toml 却没重新
// 生成 lock,cargo 会在下一次带 --locked 的调用(fetch / check / build)上报
// "cannot update the lock file",而那时往往已经在 CI runner 上、离发版只差一步。
// 2026-10-20 的 v0.1.45 就因为这个在 runner 上白跑一次(release workflow 第 8 步挂),
// 而且本地复现不了是因为本地早已 cargo fetch 过、lock 被悄悄改好了。
// 治本不靠记住:这里直接断言,漏了就在本地这步红掉。
const lock = fs.readFileSync(path.join(src, 'Cargo.lock'), 'utf8');
const lockVersion = (lock.match(/name = "tokenscope"\r?\nversion = "([^"]+)"/) || [])[1];
if (lockVersion !== version) {
  console.error(`Cargo.lock 里 tokenscope 的版本号是 ${lockVersion},与 tauri.conf.json=${version} 不一致`);
  console.error('升版本后要在 app/src-tauri 里跑一次 `cargo fetch` 让 lock 跟上');
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
// tag 没打(或打错)时清单照样生成得出,发出去就是 releases/download/vX.Y.Z/... 404,
// 老用户点更新直接失败(RELEASE.md 第 2 步在生成清单之前,所以这里可以硬校验)。
try {
  require('child_process').execSync(`git rev-parse --verify refs/tags/${tag}`, { stdio: 'pipe' });
} catch {
  console.error(`本地没有 tag ${tag} —— 先按 RELEASE.md 打 tag 再生成清单,否则发布资源全是 404`);
  process.exit(1);
}
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

// 更新说明两种给法:一串命令行参数(RELEASE.md 的用法),或 `--notes-file <路径>`。
// 后者的存在理由:这段文字要**原样**进应用里的更新卡,而多行 / 中文 / 带引号的说明
// 走命令行时会被 shell 的引号与代码页规则搅坏(实测在 PowerShell 5.1 下尤其容易)。
const argv = process.argv.slice(2);
const nfIdx = argv.indexOf('--notes-file');
let notes = '';
if (nfIdx >= 0) {
  const notesPath = argv[nfIdx + 1];
  if (!notesPath) {
    console.error('--notes-file 后面要跟一个文件路径');
    process.exit(1);
  }
  try {
    notes = fs.readFileSync(notesPath, 'utf8').trim();
  } catch (e) {
    console.error(`读不到说明文件 ${notesPath}: ${e.message}`);
    process.exit(1);
  }
  if (!notes) {
    console.error(`说明文件是空的:${notesPath}`);
    process.exit(1);
  }
} else {
  notes = argv.join(' ');
}

const json = {
  version,
  notes: notes || `Grandettoken ${version}`,
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
