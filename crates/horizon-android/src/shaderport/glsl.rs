//! Just enough GLSL scanning for the rewrites: identifiers, matching brackets, top-level
//! commas, and comments skipped.

/// Byte ranges of `// ...` and `/* ... */` comments in `text`.
pub fn comment_ranges(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'/' && b[i + 1] == b'/' {
            let end = text[i..].find('\n').map_or(b.len(), |n| i + n);
            out.push((i, end));
            i = end;
        } else if b[i] == b'/' && b[i + 1] == b'*' {
            let end = text[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
            out.push((i, end));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

pub fn in_comment(ranges: &[(usize, usize)], at: usize) -> bool {
    ranges.iter().any(|&(s, e)| at >= s && at < e)
}

pub fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Starts of whole-word occurrences of `word` outside comments.
pub fn find_word(text: &str, word: &str) -> Vec<usize> {
    let comments = comment_ranges(text);
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(n) = text[from..].find(word) {
        let at = from + n;
        let end = at + word.len();
        let before_ok = at == 0 || !is_ident_byte(b[at - 1]);
        let after_ok = end >= b.len() || !is_ident_byte(b[end]);
        if before_ok && after_ok && !in_comment(&comments, at) {
            out.push(at);
        }
        from = end;
    }
    out
}

/// The index of the bracket closing the one at `open` (`(`, `[` or `{`), skipping comments.
pub fn matching(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let (o, c) = match b[open] {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
        _ => return None,
    };
    let mut depth = 0usize;
    let mut i = open;
    while i < b.len() {
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            i = text[i..].find('\n').map_or(b.len(), |n| i + n);
            continue;
        }
        if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            i = text[i + 2..].find("*/").map_or(b.len(), |n| i + 2 + n + 2);
            continue;
        }
        if b[i] == o {
            depth += 1;
        } else if b[i] == c {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// The arguments between `(` at `open` and its `)` at `close`, split at top-level commas.
pub fn split_args(text: &str, open: usize, close: usize) -> Vec<String> {
    let inner = &text[open + 1..close];
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in inner.chars() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur).trim().to_owned());
                continue;
            }
            _ => {}
        }
        cur.push(ch);
    }
    if !cur.trim().is_empty() || !out.is_empty() {
        out.push(cur.trim().to_owned());
    }
    out
}

/// Where declarations can go in a stage file: after `#version` and any `#extension` right
/// below it (an `#extension` must come before code). `None` without a `#version`.
pub fn after_version(text: &str) -> Option<usize> {
    let mut pos = 0;
    let mut found = None;
    for line in text.split_inclusive('\n') {
        let t = line.trim_start();
        let end = pos + line.len();
        if t.starts_with("#version") {
            found = Some(end);
        } else if found.is_some() {
            if t.starts_with("#extension") {
                found = Some(end);
            } else if !(t.is_empty() || t.starts_with("//")) {
                break;
            }
        }
        pos = end;
    }
    found
}
