//! 极小的 YAML 子集:块式 mapping / 块式 sequence / 标量,外加**行范围编辑**。
//!
//! 为什么自己写、不引 crate:
//!
//! ① 要动的是**用户手写的** `cordis.patch.yml` 与 `.credentials.yaml` —— 注释、空行、
//!    `!!js` 表达式都得原样留着。整树往返(parse → 改 → serialize)会把注释与排版全丢掉,
//!    而 DSH 自己的 config-editor 明确承诺「保留替换值以外的 YAML 注释和 `!!js` 表达式」;
//!    挂件在同一份文件上写东西,口径不能比它松。
//! ② 只需要"够用"的形状:块式 mapping / 块式 sequence / 标量。流式集合(`{...}` / `[...]`)、
//!    锚点、标签一律当**不透明标量**看待;哪天真要在那里取结构,就明确报错 ——
//!    宁可不写,也不替用户改坏(与另四家客户端「认不出来就放弃」同规矩)。
//!
//! 每个节点的 `start`/`end` 是原始行数组上的半开区间 `[start, end)`,编辑就是对这个区间
//! 做替换/插入/删除 —— 区间之外的行(含注释与空行)一个字节都不动。
//!
//! 行尾:解析时按行保留(`split_inclusive('\n')`),插入的新行用文件自己的行尾
//! (`\r\n` 优先),所以混合行尾的文件也不会被我们统一成一种。

use std::ops::Range;

/// 节点形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mapping,
    Sequence,
    Scalar,
}

/// 一个节点。`key` 只在 mapping 的子节点上有值;sequence 的项没有 key。
#[derive(Debug, Clone)]
pub struct Node {
    pub kind: Kind,
    pub key: Option<String>,
    /// 标量值(去引号、去行内注释后的文本)。空值(null)是 `None` 与「键后面什么都没有」的区别:
    /// 两者都记成 `None`,因为本模块的调用方只在"要读一个标量"时才看它。
    pub value: Option<String>,
    /// 这个节点自己的缩进列(mapping 的键列)。
    pub indent: usize,
    pub start: usize,
    pub end: usize,
    pub children: Vec<Node>,
}

impl Node {
    /// 取 mapping 里的一个键(sequence / scalar 上没有子键 → None)。
    pub fn get(&self, key: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.key.as_deref() == Some(key))
    }

    /// sequence 的项。
    pub fn items(&self) -> &[Node] {
        &self.children
    }

    /// 标量文本。
    pub fn scalar(&self) -> Option<&str> {
        self.value.as_deref()
    }

    pub fn as_mapping(&self) -> Result<&Node, String> {
        if self.kind == Kind::Mapping {
            Ok(self)
        } else {
            Err(format!("{}不是块式 mapping,认不出来", self.what()))
        }
    }

    pub fn as_sequence(&self) -> Result<&Node, String> {
        if self.kind == Kind::Sequence {
            Ok(self)
        } else {
            Err(format!("{}不是块式 sequence,认不出来", self.what()))
        }
    }

    /// 报错时的自称:有键就报键名,否则报行号。
    pub fn what(&self) -> String {
        match &self.key {
            Some(k) => format!("「{k}」"),
            None => format!("第 {} 行那一段", self.start + 1),
        }
    }

    /// 这个节点占了哪几行(显示用,1 起)。
    pub fn lines(&self) -> String {
        format!("{}-{}", self.start + 1, self.end)
    }
}

// ───────────────────────── 解析 ─────────────────────────

/// 按行切开,保留每行自己的行尾。
pub fn split(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.split_inclusive('\n').map(|s| s.to_string()).collect()
}

/// 拼回去。
pub fn join(lines: &[String]) -> String {
    lines.concat()
}

/// 这个文件的行尾(`\r\n` 优先)。插入的新行照它写。
pub fn eol_of(lines: &[String]) -> String {
    if lines.iter().any(|l| l.ends_with("\r\n")) {
        "\r\n".to_string()
    } else {
        "\n".to_string()
    }
}

/// 一行去掉行尾之后的正文。
fn content(raw: &str) -> &str {
    raw.trim_end_matches('\n').trim_end_matches('\r')
}

fn is_blank_or_comment(c: &str) -> bool {
    let t = c.trim();
    t.is_empty() || t.starts_with('#')
}

/// 内容行的缩进列。空行/注释行没有缩进(返回 None)。
/// 缩进里出现制表符 → None(调用方按"认不出来"处理,不当成缩进)。
fn indent_of(c: &str) -> Option<usize> {
    if is_blank_or_comment(c) {
        return None;
    }
    let lead = c.len() - c.trim_start_matches(' ').len();
    if c[lead..].starts_with('\t') {
        return None;
    }
    Some(lead)
}

fn next_content(lines: &[String], mut i: usize) -> usize {
    while i < lines.len() && is_blank_or_comment(content(&lines[i])) {
        i += 1;
    }
    i
}

fn tab_err(i: usize) -> String {
    format!("第 {} 行的缩进用了制表符,认不出来", i + 1)
}

/// 解析整份文件。空文件得到一棵空的 mapping 根。
pub fn parse(lines: &[String]) -> Result<Node, String> {
    let i = next_content(lines, 0);
    let mut root = Node {
        kind: Kind::Mapping,
        key: None,
        value: None,
        indent: 0,
        start: 0,
        end: lines.len(),
        children: Vec::new(),
    };
    if i >= lines.len() {
        return Ok(root);
    }
    let c = content(&lines[i]);
    let Some(ind) = indent_of(c) else {
        return Err(tab_err(i));
    };
    if ind != 0 {
        return Err(format!(
            "第 {} 行是缩进的,但文件根节点应该顶格,认不出来",
            i + 1
        ));
    }
    let (node, _) = parse_block(lines, i, 0)?;
    root.kind = node.kind;
    root.children = node.children;
    root.start = node.start;
    Ok(root)
}

/// 一段块:内容行的缩进必须正好是 `indent`(调用方已确认第 `i` 行是内容行)。
fn parse_block(lines: &[String], i: usize, indent: usize) -> Result<(Node, usize), String> {
    let c = content(&lines[i]);
    if starts_dash(c, indent) {
        parse_sequence(lines, i, indent)
    } else {
        parse_mapping(lines, (i, indent), indent)
    }
}

/// 第 `indent` 列是不是 sequence 的 `-`。
fn starts_dash(c: &str, indent: usize) -> bool {
    let b = c.as_bytes();
    if indent >= b.len() || b[indent] != b'-' {
        return false;
    }
    indent + 1 >= b.len() || b[indent + 1] == b' '
}

fn parse_sequence(lines: &[String], i0: usize, indent: usize) -> Result<(Node, usize), String> {
    let mut items: Vec<Node> = Vec::new();
    let mut i = i0;
    loop {
        if i >= lines.len() {
            break;
        }
        let c = content(&lines[i]);
        if is_blank_or_comment(c) {
            i += 1;
            continue;
        }
        let Some(ind) = indent_of(c) else {
            return Err(tab_err(i));
        };
        if ind < indent {
            break;
        }
        if ind > indent {
            return Err(format!("第 {} 行的缩进对不上,认不出来", i + 1));
        }
        if !starts_dash(c, ind) {
            // 同列的普通行:这个 sequence 到头了。列表与键同列时(`models:` 下一行 `- x`),
            // 紧跟其后的兄弟键就长这样 —— 交回给外层 mapping 接着解析。
            break;
        }
        let after = &c[ind + 1..];
        let rest = after.trim_start();
        let inner = ind + 1 + (after.len() - rest.len());
        if rest.is_empty() || rest.starts_with('#') {
            // 光一个 `-`:子块在下一行
            let j = next_content(lines, i + 1);
            if j < lines.len() {
                if let Some(ij) = indent_of(content(&lines[j])) {
                    if ij > ind {
                        let (child, nj) = parse_block(lines, j, ij)?;
                        items.push(Node {
                            kind: child.kind,
                            key: None,
                            value: None,
                            // 项的缩进一律记**短横所在列**(重排时要照它写回去)
                            indent: ind,
                            start: i,
                            end: child.end,
                            children: child.children,
                        });
                        i = nj;
                        continue;
                    }
                }
            }
            items.push(Node {
                kind: Kind::Scalar,
                key: None,
                value: None,
                indent: ind,
                start: i,
                end: i + 1,
                children: Vec::new(),
            });
            i += 1;
            continue;
        }
        if split_key(rest).is_some() {
            // `- key: value`:行内起的 mapping 项,后续键与它同列
            let (mut node, ni) = parse_mapping(lines, (i, inner), inner)?;
            if ni <= i {
                // 兜底:游标不前进 = 一定会死循环(真机踩过一次),宁可明确报错
                return Err(format!("第 {} 行的项解析不出内容,认不出来", i + 1));
            }
            node.key = None;
            // 项的缩进 = 短横所在列(不是键列):重排这条项时要照它写回去
            node.indent = ind;
            node.start = i;
            items.push(node);
            i = ni;
            continue;
        }
        items.push(Node {
            kind: Kind::Scalar,
            key: None,
            value: Some(strip_scalar(rest)),
            indent: ind,
            start: i,
            end: i + 1,
            children: Vec::new(),
        });
        i += 1;
    }
    let end = items.last().map(|x| x.end).unwrap_or(i0 + 1);
    Ok((
        Node {
            kind: Kind::Sequence,
            key: None,
            value: None,
            indent,
            start: i0,
            end,
            children: items,
        },
        i,
    ))
}

/// 一段 mapping。`first` = 第一个键所在的行与列(sequence 的 `- key:` 项就从列 `indent` 起算)。
fn parse_mapping(
    lines: &[String],
    first: (usize, usize),
    indent: usize,
) -> Result<(Node, usize), String> {
    let start_line = first.0;
    let mut children: Vec<Node> = Vec::new();
    let mut i = first.0;
    let col = first.1;
    loop {
        if i >= lines.len() {
            break;
        }
        let c = content(&lines[i]);
        if is_blank_or_comment(c) {
            i += 1;
            continue;
        }
        // 第一行是**行内起头**的(`- key: value`):它的"缩进"是 `-` 所在的列,
        // 比键列浅,所以第一轮不能拿它跟 indent 比 —— 否则当场 break、游标不前进,
        // 外层序列会一直往里塞同一个空节点(真机踩过:内存几分钟吃光)。
        let is_first = i == first.0;
        let Some(ind) = indent_of(c) else {
            return Err(tab_err(i));
        };
        if !is_first {
            if ind < indent {
                break;
            }
            if ind > indent {
                return Err(format!("第 {} 行的缩进比上一行深,认不出来", i + 1));
            }
        }
        if col > c.len() {
            return Err(format!("第 {} 行的内容对不上列,认不出来", i + 1));
        }
        let text = &c[col..];
        let Some((key, rest)) = split_key(text) else {
            return Err(format!("第 {} 行不是「键: 值」,认不出来", i + 1));
        };
        let rest = rest.trim_start();
        let mut node = Node {
            kind: Kind::Scalar,
            key: Some(key),
            value: None,
            indent,
            start: i,
            end: i + 1,
            children: Vec::new(),
        };
        if rest.is_empty() || rest.starts_with('#') {
            // 值在下一行(更深的缩进)才叫块;否则是空值。
            // 两种合法写法都收:短横比键深(`models:` 下一行 `  - id`),或与键同列
            // (`models:` 下一行 `- id`,YAML 也认,别家工具会这么吐)。
            let j = next_content(lines, i + 1);
            let mut nested = false;
            if j < lines.len() {
                if let Some(ij) = indent_of(content(&lines[j])) {
                    let same_indent_seq = ij == indent && starts_dash(content(&lines[j]), ij);
                    if ij > indent || same_indent_seq {
                        let (child, nj) = parse_block(lines, j, ij)?;
                        node.kind = child.kind;
                        node.children = child.children;
                        node.end = child.end;
                        i = nj;
                        nested = true;
                    }
                }
            }
            if !nested {
                i += 1;
            }
            children.push(node);
            continue;
        }
        node.value = Some(strip_scalar(rest));
        let block_scalar = rest.starts_with('|') || rest.starts_with('>');
        i += 1;
        if block_scalar {
            // 块标量(`|` / `>`):它下面那些更深的行都是内容,不能当兄弟键解析
            while i < lines.len() {
                let Some(ix) = indent_of(content(&lines[i])) else {
                    break;
                };
                if ix <= indent {
                    break;
                }
                i += 1;
            }
            node.end = i;
        }
        children.push(node);
    }
    let end = children.last().map(|x| x.end).unwrap_or(start_line + 1);
    Ok((
        Node {
            kind: Kind::Mapping,
            key: None,
            value: None,
            indent,
            start: start_line,
            end,
            children,
        },
        i,
    ))
}

/// 一行是不是「键: 值」,是就切开。引号键认,键里的 `:`(后面不跟空格)不当分隔符。
fn split_key(text: &str) -> Option<(String, &str)> {
    let b = text.as_bytes();
    if b.is_empty() {
        return None;
    }
    if b[0] == b'"' || b[0] == b'\'' {
        let q = b[0];
        let mut k = 1;
        while k < b.len() && b[k] != q {
            k += 1;
        }
        if k >= b.len() {
            return None;
        }
        let key = strip_scalar(&text[..=k]);
        let rest = text[k + 1..].strip_prefix(':')?;
        return Some((key, rest));
    }
    for k in 0..b.len() {
        if b[k] != b':' {
            continue;
        }
        if k + 1 < b.len() && b[k + 1] != b' ' {
            continue;
        }
        let key = text[..k].trim();
        if key.is_empty() {
            return None;
        }
        return Some((key.to_string(), &text[k + 1..]));
    }
    None
}

/// 标量文本:去引号、去行内注释;流式集合 / 标签 / 锚点一律原样(当不透明)。
fn strip_scalar(raw: &str) -> String {
    let t = raw.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        return t[1..t.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\");
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        return t[1..t.len() - 1].replace("''", "'");
    }
    let first = t.chars().next().unwrap_or(' ');
    if matches!(first, '!' | '{' | '[' | '&' | '*' | '|' | '>') {
        return t.to_string();
    }
    // YAML 里 ` #` 才是注释起点;值里带 # 但不跟空格的(如 `a#b`)是正文
    if let Some(p) = t.find(" #") {
        return t[..p].trim_end().to_string();
    }
    t.to_string()
}

/// 把一个字符串写成 YAML 标量。普通密钥 / URL 直接裸写,其余加双引号转义。
pub fn scalar_text(v: &str) -> String {
    let plain = !v.is_empty()
        && v == v.trim()
        && !v.starts_with('-')
        && !v.starts_with(':')
        && v.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '.' | '_' | '-' | '/' | ':' | '@' | '+' | '=' | '(' | ')' | ',' | '~')
        });
    if plain {
        v.to_string()
    } else {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

// ───────────────────────── 编辑 ─────────────────────────

/// 用 `new_lines`(不含行尾)替换 `range` 这几行。区间外的行原样不动。
pub fn splice(lines: &mut Vec<String>, range: Range<usize>, new_lines: &[String]) {
    if new_lines.is_empty() && range.is_empty() {
        return;
    }
    let eol = eol_of(lines);
    // 末尾有没有换行要**先看**:下面给前一行补行尾会改掉这个事实
    let file_ends_with_nl = lines.last().map(|l| l.ends_with('\n')).unwrap_or(true);
    let at_eof = range.end >= lines.len();
    // 插进去的前一行如果缺行尾(文件末尾没换行的老文件),先补上 ——
    // 否则新内容会与它粘成一行,直接把配置写坏
    if !new_lines.is_empty()
        && range.start > 0
        && range.start <= lines.len()
        && !lines[range.start - 1].ends_with('\n')
    {
        lines[range.start - 1].push_str(&eol);
    }
    let mut out: Vec<String> = Vec::with_capacity(new_lines.len() + 4);
    for (n, l) in new_lines.iter().enumerate() {
        let last = n + 1 == new_lines.len();
        if last && at_eof && !file_ends_with_nl {
            out.push(l.clone());
        } else {
            out.push(format!("{l}{eol}"));
        }
    }
    // 区间上界钳一下:调用方给的都是合法区间,但 release 用 panic="abort" —— 哪天传越界了,
    // 应该是"这次写入失败",而不是整个挂件原地消失。
    let start = range.start.min(lines.len());
    let end = range.end.max(start).min(lines.len());
    lines.splice(start..end, out);
}

/// 把一段内容按 `pad` 列缩进(用于整块插入)。
pub fn indented(lines: &[String], pad: usize) -> Vec<String> {
    let spaces = " ".repeat(pad);
    lines
        .iter()
        .map(|l| if l.is_empty() { l.clone() } else { format!("{spaces}{l}") })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一份与真机同形的 patch 文件:注释、空行、引号值、URL 值、嵌套 sequence 都在。
    /// 注意用原始字符串:**Rust 的行续接 `\` 会吃掉下一行的缩进**,夹具会被悄悄改写,
    /// 这条踩过一次(解析器因此报"缩进对不上",查了半天)。
    const PATCH: &str = r#"# dsh profile patch
# 第二行注释
- id: agent-default-model
  name: "@deepseek-ai/dsh-agent-default-model"
  config:
    provider: opencode-go
    model: deepseek-v4.1-flash
    reasoningEffort: high

- id: llm-pi-ai
  name: "@deepseek-ai/dsh-llm-pi-ai"
  config:
    providers:
      opencode-go:
        models:
          - id: deepseek-v4.1-flash
            name: OG DeepSeek V4.1 Flash
            contextWindow: 1000000
            maxTokens: 384000
            input:
              - text
              - image
        apiKeyEnv: OPENCODE_GO_API_KEY
"#;

    fn parsed() -> Node {
        parse(&split(PATCH)).unwrap()
    }

    #[test]
    fn parses_top_level_entries() {
        let root = parsed();
        assert_eq!(root.kind, Kind::Sequence);
        assert_eq!(root.children.len(), 2);
        let e0 = &root.children[0];
        assert_eq!(e0.kind, Kind::Mapping);
        assert_eq!(e0.get("id").unwrap().scalar(), Some("agent-default-model"));
        assert_eq!(e0.get("name").unwrap().scalar(), Some("@deepseek-ai/dsh-agent-default-model"));
        let cfg = e0.get("config").unwrap();
        assert_eq!(cfg.get("provider").unwrap().scalar(), Some("opencode-go"));
        assert_eq!(cfg.get("model").unwrap().scalar(), Some("deepseek-v4.1-flash"));
    }

    #[test]
    fn parses_nested_sequence_of_mappings() {
        let root = parsed();
        let llm = root
            .children
            .iter()
            .find(|e| e.get("id").and_then(|n| n.scalar()) == Some("llm-pi-ai"))
            .expect("llm-pi-ai 那条要在");
        let providers = llm.get("config").unwrap().get("providers").unwrap();
        assert_eq!(providers.kind, Kind::Mapping);
        let og = providers.get("opencode-go").unwrap();
        assert_eq!(og.get("apiKeyEnv").unwrap().scalar(), Some("OPENCODE_GO_API_KEY"));
        let models = og.get("models").unwrap();
        assert_eq!(models.kind, Kind::Sequence);
        assert_eq!(models.items().len(), 1);
        let m = &models.items()[0];
        assert_eq!(m.get("id").unwrap().scalar(), Some("deepseek-v4.1-flash"));
        assert_eq!(m.get("name").unwrap().scalar(), Some("OG DeepSeek V4.1 Flash"));
        assert_eq!(m.get("contextWindow").unwrap().scalar(), Some("1000000"));
        let input = m.get("input").unwrap();
        assert_eq!(input.kind, Kind::Sequence);
        assert_eq!(
            input.items().iter().map(|x| x.scalar().unwrap()).collect::<Vec<_>>(),
            vec!["text", "image"]
        );
    }

    /// URL 里的冒号不能被当成分隔符;行内注释要被剥掉。
    #[test]
    fn scalar_values_with_colons_and_comments() {
        let text = "a: https://4sapi.com/v1\nb: sk-abc # 这是我的密钥\nc: \"x: y\"\n";
        let root = parse(&split(text)).unwrap();
        assert_eq!(root.get("a").unwrap().scalar(), Some("https://4sapi.com/v1"));
        assert_eq!(root.get("b").unwrap().scalar(), Some("sk-abc"));
        assert_eq!(root.get("c").unwrap().scalar(), Some("x: y"));
    }

    /// 行号区间要准:替换一条 route,其它条目与注释一个字节都不能动。
    #[test]
    fn ranges_let_us_replace_one_block_only() {
        let mut lines = split(PATCH);
        let root = parse(&lines).unwrap();
        let llm = root
            .children
            .iter()
            .find(|e| e.get("id").and_then(|n| n.scalar()) == Some("llm-pi-ai"))
            .unwrap();
        let providers = llm.get("config").unwrap().get("providers").unwrap();
        let og = providers.get("opencode-go").unwrap();
        assert_eq!(og.start, 13, "providers 下的第一条在 0 起的第 13 行");
        let new = vec![
            "      opencode-go:".to_string(),
            "        apiKeyEnv: OTHER".to_string(),
        ];
        splice(&mut lines, og.start..og.end, &new);
        let out = join(&lines);
        assert!(out.contains("# 第二行注释"), "注释必须留着");
        assert!(out.contains("- id: agent-default-model"), "别的条目必须留着");
        assert!(out.contains("apiKeyEnv: OTHER"));
        assert!(!out.contains("OPENCODE_GO_API_KEY"), "旧的那条该被换掉");
        // 换完还能再解析,且新值读得回来
        let again = parse(&split(&out)).unwrap();
        let llm2 = again
            .children
            .iter()
            .find(|e| e.get("id").and_then(|n| n.scalar()) == Some("llm-pi-ai"))
            .unwrap();
        assert_eq!(
            llm2.get("config")
                .unwrap()
                .get("providers")
                .unwrap()
                .get("opencode-go")
                .unwrap()
                .get("apiKeyEnv")
                .unwrap()
                .scalar(),
            Some("OTHER")
        );
    }

    /// 在 mapping 末尾插入:插到最后一个子节点之后,不留空行错位。
    #[test]
    fn splice_inserts_at_mapping_end() {
        let mut lines = split(PATCH);
        let root = parse(&lines).unwrap();
        let llm = &root.children[1];
        let providers = llm.get("config").unwrap().get("providers").unwrap();
        let at = providers.end;
        splice(
            &mut lines,
            at..at,
            &[
                "      hapi:".to_string(),
                "        api: openai-completions".to_string(),
            ],
        );
        let out = join(&lines);
        let root2 = parse(&split(&out)).unwrap();
        let prov2 = root2.children[1]
            .get("config")
            .unwrap()
            .get("providers")
            .unwrap();
        assert_eq!(prov2.children.len(), 2, "新增的 route 要并进 providers");
        assert_eq!(
            prov2.get("hapi").unwrap().get("api").unwrap().scalar(),
            Some("openai-completions")
        );
        assert_eq!(
            prov2.get("opencode-go").unwrap().get("apiKeyEnv").unwrap().scalar(),
            Some("OPENCODE_GO_API_KEY"),
            "原有 route 不受影响"
        );
    }

    /// CRLF 文件:插入的新行也用 CRLF,旧行原样。
    #[test]
    fn crlf_is_preserved() {
        let text = "version: 1\r\nrefs:\r\n  A: one\r\n";
        let mut lines = split(text);
        assert_eq!(eol_of(&lines), "\r\n");
        let root = parse(&lines).unwrap();
        let refs = root.get("refs").unwrap();
        splice(&mut lines, refs.end..refs.end, &["  B: two".to_string()]);
        assert_eq!(join(&lines), "version: 1\r\nrefs:\r\n  A: one\r\n  B: two\r\n");
    }

    /// 文件末尾没有换行时,别硬塞一个进去。
    #[test]
    fn missing_trailing_newline_is_kept() {
        let mut lines = split("a: 1");
        assert_eq!(join(&lines), "a: 1");
        splice(&mut lines, 1..1, &["b: 2".to_string()]);
        assert_eq!(join(&lines), "a: 1\nb: 2");
    }

    /// 流式集合与 `!!js` 当不透明标量 —— 想在那里取结构就明确报错,而不是猜。
    #[test]
    fn flow_and_tags_are_opaque() {
        let text = "a: {x: 1}\nb: !!js some.thing\nc: [1, 2]\n";
        let root = parse(&split(text)).unwrap();
        assert_eq!(root.get("a").unwrap().scalar(), Some("{x: 1}"));
        assert_eq!(root.get("b").unwrap().scalar(), Some("!!js some.thing"));
        assert_eq!(root.get("c").unwrap().scalar(), Some("[1, 2]"));
        assert!(root.get("a").unwrap().as_mapping().is_err(), "流式集合不是块式 mapping");
    }

    /// 制表符缩进、缩进跳跃:一律认不出来(宁可不写)。
    #[test]
    fn rejects_tabs_and_bad_indent() {
        assert!(parse(&split("a:\n\tb: 1\n")).is_err());
        assert!(parse(&split("a: 1\n    b: 2\n")).is_err());
        assert!(parse(&split("  a: 1\n")).is_err());
    }

    /// 两种合法的列表写法都要认:短横比键深(手写常见),或与键同列(别家工具会这么吐)。
    #[test]
    fn accepts_both_sequence_indent_styles() {
        let deep = "models:\n  - id: a\n    name: A\napiKeyEnv: K\n";
        let r = parse(&split(deep)).unwrap();
        let m = r.get("models").unwrap();
        assert_eq!(m.kind, Kind::Sequence);
        assert_eq!(m.items().len(), 1);
        assert_eq!(m.items()[0].get("name").unwrap().scalar(), Some("A"));
        assert_eq!(r.get("apiKeyEnv").unwrap().scalar(), Some("K"), "列表后面的兄弟键要读得到");

        let same = "models:\n- id: a\n  name: A\napiKeyEnv: K\n";
        let r2 = parse(&split(same)).unwrap();
        let m2 = r2.get("models").unwrap();
        assert_eq!(m2.kind, Kind::Sequence);
        assert_eq!(m2.items().len(), 1);
        assert_eq!(m2.items()[0].get("name").unwrap().scalar(), Some("A"));
        assert_eq!(
            r2.get("apiKeyEnv").unwrap().scalar(),
            Some("K"),
            "同列写法下,后面的兄弟键同样要读得到"
        );
        // 项的缩进一律记短横所在列(重排时要照它写回去)
        assert_eq!(m.items()[0].indent, 2);
        assert_eq!(m2.items()[0].indent, 0);
    }

    /// 空文件、纯注释文件、块标量都要能平安过。
    #[test]
    fn edge_inputs() {
        assert!(parse(&split("")).unwrap().children.is_empty());
        assert!(parse(&split("# 只有注释\n\n")).unwrap().children.is_empty());
        let text = "a: |\n  line one\n  line two\nb: 2\n";
        let root = parse(&split(text)).unwrap();
        assert_eq!(root.get("a").unwrap().scalar(), Some("|"));
        assert_eq!(root.get("b").unwrap().scalar(), Some("2"), "块标量后面的兄弟键要读得到");
    }

    /// 标量输出:普通密钥/URL 裸写,带空格或特殊字符的加引号。
    #[test]
    fn scalar_text_quoting() {
        assert_eq!(scalar_text("sk-abc123"), "sk-abc123");
        assert_eq!(scalar_text("https://4sapi.com/v1"), "https://4sapi.com/v1");
        assert_eq!(scalar_text("4S-claude sonnet"), "\"4S-claude sonnet\"");
        assert_eq!(scalar_text("has\"quote"), "\"has\\\"quote\"");
        assert_eq!(scalar_text("-leading"), "\"-leading\"");
        assert_eq!(scalar_text(""), "\"\"");
        // 输出必须能被自己读回来
        let mut lines = split(&format!("a: {}\n", scalar_text("x: y # z")));
        let root = parse(&lines).unwrap();
        assert_eq!(root.get("a").unwrap().scalar(), Some("x: y # z"));
        splice(&mut lines, 0..1, &[]);
        assert_eq!(join(&lines), "");
    }
}
