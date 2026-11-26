const fs = require("fs");
const path = require("path");
const vm = require("vm");

const DEFAULT_SRC = path.resolve(__dirname, "../../app/src");
const BOOTSTRAP_NAMES = ["gt-api.js", "gt-store.js", "gt-window-state.js"];

function appFileNames(srcDir = DEFAULT_SRC) {
  const htmlPath = path.join(srcDir, "index.html");
  const html = fs.readFileSync(htmlPath, "utf8");
  const re = /<script\b[^>]*\bsrc="([^"]+)"[^>]*>\s*<\/script>/g;
  const names = [];
  let m;
  while ((m = re.exec(html)) !== null) {
    const src = m[1];
    if (/^app(?:-.+)?\.js$/.test(src)) {
      names.push(src);
    }
  }
  if (!names.length) {
    throw new Error(`未在 ${htmlPath} 中找到 app 脚本标签`);
  }
  return names;
}

function readScripts(srcDir, names) {
  return names.map((name) => {
    const filePath = path.join(srcDir, name);
    return {
      name,
      path: filePath,
      code: fs.readFileSync(filePath, "utf8"),
    };
  });
}

function bootstrapFiles(srcDir = DEFAULT_SRC) {
  const html = fs.readFileSync(path.join(srcDir, "index.html"), "utf8");
  let previous = -1;
  for (const name of BOOTSTRAP_NAMES) {
    const tag = `src="${name}"`;
    const index = html.indexOf(tag);
    if (index <= previous || index >= html.indexOf('src="app-core.js"') || html.indexOf(tag, index + 1) !== -1) {
      throw new Error(`前置脚本顺序错误或重复: ${name}`);
    }
    previous = index;
  }
  return readScripts(srcDir, BOOTSTRAP_NAMES);
}

function appFiles(srcDir = DEFAULT_SRC) {
  return [...bootstrapFiles(srcDir), ...readScripts(srcDir, appFileNames(srcDir))];
}

function apiSource(srcDir = DEFAULT_SRC) {
  return fs.readFileSync(path.join(srcDir, BOOTSTRAP_NAMES[0]), "utf8");
}

function baseStyleSource(srcDir = DEFAULT_SRC) {
  return ["styles-tokens.css", "styles-panel.css", "styles-manage.css"]
    .map((name) => fs.readFileSync(path.join(srcDir, name), "utf8")).join("\n");
}

function appSource(srcDirOrFiles = DEFAULT_SRC) {
  const files = Array.isArray(srcDirOrFiles) ? srcDirOrFiles : appFiles(srcDirOrFiles);
  return files.map((f) => f.code).join("");
}

function countOccurrences(haystack, needle) {
  if (!needle) throw new Error("replaceOnce 的 marker 不能为空");
  return haystack.split(needle).length - 1;
}

function replaceOnce(target, marker, replacement) {
  if (typeof target === "string") {
    const count = countOccurrences(target, marker);
    if (count !== 1) {
      throw new Error(`replaceOnce 期望唯一命中 1 处，实际命中 ${count} 处: ${marker}`);
    }
    return target.replace(marker, () => replacement);
  }
  if (Array.isArray(target)) {
    let total = 0;
    let hitFile = null;
    for (const f of target) {
      const c = countOccurrences(f.code, marker);
      if (c > 0) {
        total += c;
        hitFile = f;
      }
    }
    if (total !== 1 || !hitFile) {
      throw new Error(`replaceOnce 期望在脚本列表中唯一命中 1 处，实际命中 ${total} 处: ${marker}`);
    }
    hitFile.code = hitFile.code.replace(marker, () => replacement);
    return target;
  }
  throw new Error("replaceOnce 的 target 必须是字符串或 appFiles() 数组");
}

function loadInto(context, files) {
  for (const item of files) {
    const name = typeof item === "string" ? "<inline>" : item.name || "<script>";
    const code = typeof item === "string" ? item : item.code;
    new vm.Script(code, { filename: name }).runInContext(context);
  }
}

module.exports = {
  DEFAULT_SRC,
  appFileNames,
  appFiles,
  appSource,
  bootstrapFiles,
  apiSource,
  baseStyleSource,
  replaceOnce,
  loadInto,
};
