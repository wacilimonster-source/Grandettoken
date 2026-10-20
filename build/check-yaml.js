// 轻量 YAML 语法检查:不引第三方依赖(本仓库刻意无 node_modules)。
// 只做"能不能解析"这一层 —— 结构语义仍由 GitHub Actions 自己报。
// 这个子集覆盖 YAML 的一常用法:注释 / 键值 / 嵌套缩进 / 行内块标量(| >)/ 列表。
const fs = require('fs');

function parseYaml(src) {
  const lines = src.split(/\r?\n/);
  const root = {};
  // 栈里每项:{ indent, container }
  const stack = [{ indent: -1, container: root }];
  let i = 0;

  const scalar = (raw) => {
    let s = raw.trim();
    if (s === '' || s === '~' || s === 'null') return null;
    if (s === 'true') return true;
    if (s === 'false') return false;
    if (/^-?\d+$/.test(s)) return Number(s);
    if (/^-?\d+\.\d+$/.test(s)) return Number(s);
    if ((s.startsWith('"') && s.endsWith('"')) || (s.startsWith("'") && s.endsWith("'"))) {
      return s.slice(1, -1);
    }
    if (s.startsWith('[') && s.endsWith(']')) {
      const inner = s.slice(1, -1).trim();
      if (!inner) return [];
      return inner.split(',').map((x) => scalar(x));
    }
    return s;
  };

  while (i < lines.length) {
    const line = lines[i];
    const trimmed = line.trim();
    // 空行与注释
    if (trimmed === '' || trimmed.startsWith('#')) { i++; continue; }
    const indent = line.length - line.replace(/^ +/, '').length;

    if (trimmed.startsWith('- ') || trimmed === '-') {
      // 列表项:挂到最近的数组容器上
      let parent = stack[stack.length - 1];
      while (parent.container && Object.prototype.hasOwnProperty.call(parent.container, '__seq') === false
             && stack.length > 1 && stack[stack.length - 2].indent >= indent) {
        stack.pop();
        parent = stack[stack.length - 1];
      }
      const value = trimmed === '-' ? '' : trimmed.slice(2).trim();
      // 顶层或首次出现的 "-":父容器必须是 __seq
      let target = parent.container;
      // 找到带 __seq 的那一层
      let j = stack.length - 1;
      while (j >= 0 && !Array.isArray(stack[j].container.__seq)) j--;
      if (j < 0) throw new Error(`line ${i + 1}: list item outside a sequence`);
      const arr = stack[j].container.__seq;
      if (value === '') {
        const obj = {};
        arr.push(obj);
        stack.push({ indent, container: obj });
      } else if (/^[A-Za-z_][\w-]*:/.test(value)) {
        const obj = {};
        arr.push(obj);
        const k = value.slice(0, value.indexOf(':'));
        const v = value.slice(value.indexOf(':') + 1).trim();
        obj[k] = v === '' ? {} : scalar(v);
      } else {
        arr.push(scalar(value));
      }
      i++;
      continue;
    }

    const m = trimmed.match(/^([A-Za-z_"'][^:]*?):(.*)$/);
    if (!m) throw new Error(`line ${i + 1}: not a mapping entry: ${trimmed.slice(0, 60)}`);
    const key = m[1].trim().replace(/^["']|["']$/g, '');
    let rest = m[2].trim();
    if (rest.startsWith('#')) rest = '';

    // 找到该键所属的容器:缩进大于等于当前栈顶的同层键
    while (stack.length > 1 && stack[stack.length - 1].indent >= indent) stack.pop();
    const container = stack[stack.length - 1].container;

    if (rest === '|' || rest === '>' || /^[|>][+-]?$/.test(rest)) {
      // 块标量:把后续更深缩进的行原样收进来
      const buf = [];
      let j = i + 1;
      let blockIndent = null;
      while (j < lines.length) {
        const l = lines[j];
        if (l.trim() === '') { buf.push(''); j++; continue; }
        const ind = l.length - l.replace(/^ +/, '').length;
        if (ind <= indent) break;
        if (blockIndent === null) blockIndent = ind;
        buf.push(l.slice(Math.min(blockIndent, ind)));
        j++;
      }
      container[key] = buf.join('\n');
      i = j;
      continue;
    }

    if (rest === '') {
      // 可能是嵌套映射,也可能是序列(看下一行)
      let j = i + 1;
      while (j < lines.length && (lines[j].trim() === '' || lines[j].trim().startsWith('#'))) j++;
      const next = lines[j];
      const nextIndent = next ? next.length - next.replace(/^ +/, '').length : -1;
      if (next && next.trim().startsWith('- ') && nextIndent >= indent) {
        container[key] = { __seq: [] };
        // 序列的宿主层缩进记为 indent,列表项缩进更深
        stack.push({ indent, container: container[key] });
      } else {
        const obj = {};
        container[key] = obj;
        stack.push({ indent, container: obj });
      }
      i++;
      continue;
    }

    container[key] = scalar(rest);
    i++;
  }
  return root;
}

// __seq 只是为了解析时定位,校验完清掉,让结果可以直接看
function strip(node) {
  if (Array.isArray(node)) return node.map(strip);
  if (node && typeof node === 'object') {
    const out = {};
    for (const [k, v] of Object.entries(node)) out[k] = strip(v);
    return out;
  }
  return node;
}

const files = process.argv.slice(2);
let bad = 0;
for (const f of files) {
  try {
    const src = fs.readFileSync(f, 'utf8');
    const doc = strip(parseYaml(src));
    const keys = Object.keys(doc).join(', ');
    console.log(`OK    ${f}  (top-level: ${keys || '(empty)'})`);
  } catch (e) {
    bad++;
    console.log(`FAIL  ${f}  ${e.message}`);
  }
}
process.exit(bad ? 1 : 0);
