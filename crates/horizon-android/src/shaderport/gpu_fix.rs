//! Rules for what the game's GPU compile refuses (see `gpu.rs`), which the loader alone passes.

use std::collections::BTreeMap;

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Edits, sources};
use crate::shaderport::fix::guards_at;
use crate::shaderport::glsl::{comment_ranges, find_word, in_comment, is_ident_byte, matching, split_args};
use crate::shaderport::load::Eval;

// ---- out-arg-temporary ----------------------------------------------------------------------

/// A parameter of a function: written by the call (`out`/`inout`), its type.
#[derive(Clone, PartialEq)]
struct Param {
    out: bool,
    ty: Option<String>,
}

const QUALIFIERS: [&str; 9] = ["const", "in", "out", "inout", "highp", "mediump", "lowp", "precise", "flat"];

fn is_builtin_type(w: &str) -> bool {
    let vec = |p: &str| {
        w.strip_prefix(p)
            .is_some_and(|n| ["2", "3", "4"].contains(&n))
    };
    ["float", "int", "uint", "bool", "double"].contains(&w)
        || ["vec", "ivec", "uvec", "bvec", "dvec", "mat"].iter().any(|p| vec(p))
        || ["mat2x2", "mat2x3", "mat2x4", "mat3x2", "mat3x3", "mat3x4", "mat4x2", "mat4x3", "mat4x4"].contains(&w)
}

/// Struct name → member name → type, from every source.
fn struct_members(srcs: &[(String, String)]) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (_, text) in srcs {
        for at in find_word(text, "struct") {
            let Some(open) = text[at..].find('{').map(|i| at + i) else { continue };
            let name = text[at + 6..open].trim();
            if name.is_empty() || !name.bytes().all(is_ident_byte) {
                continue;
            }
            let Some(close) = matching(text, open) else { continue };
            let c = comment_ranges(&text[open + 1..close]);
            let body: String = text[open + 1..close]
                .char_indices()
                .filter(|(i, _)| !in_comment(&c, *i))
                .map(|(_, ch)| ch)
                .collect();
            let members = out.entry(name.to_owned()).or_default();
            for decl in body.split(';').map(str::trim).filter(|d| !d.is_empty() && !d.contains('#')) {
                let words: Vec<&str> = decl
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|w| !w.is_empty() && !QUALIFIERS.contains(w))
                    .collect();
                if words.len() < 2 || decl.contains('[') {
                    continue;
                }
                for n in &words[1..] {
                    members.insert((*n).to_owned(), words[0].to_owned());
                }
            }
        }
    }
    out
}

/// Every function defined or declared: name → overloads (parameters in order).
fn signatures(srcs: &[(String, String)]) -> BTreeMap<String, Vec<Vec<Param>>> {
    let mut out: BTreeMap<String, Vec<Vec<Param>>> = BTreeMap::new();
    for (_, text) in srcs {
        let b = text.as_bytes();
        let comments = comment_ranges(text);
        let mut depth = 0i32;
        let mut i = 0;
        while i < b.len() {
            if in_comment(&comments, i) {
                i += 1;
                continue;
            }
            match b[i] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                b'(' if depth == 0 => {
                    // `TYPE NAME (` ... `)` then `{` or `;`
                    let name_end = text[..i].trim_end().len();
                    let name_start = text[..name_end]
                        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .map_or(0, |p| p + 1);
                    let name = &text[name_start..name_end];
                    let before = text[..name_start].trim_end();
                    let ty_start = before
                        .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .map_or(0, |p| p + 1);
                    let ty = &before[ty_start..];
                    if let Some(close) = matching(text, i)
                        && !name.is_empty()
                        && !ty.is_empty()
                        && !["return", "else", "if", "while", "for", "switch", "define"].contains(&ty)
                        && matches!(text[close + 1..].trim_start().bytes().next(), Some(b'{' | b';'))
                    {
                        let params: Vec<Param> = split_args(text, i, close)
                            .iter()
                            .map(|p| p.trim())
                            .filter(|p| !p.is_empty() && *p != "void")
                            .map(|p| {
                                let words: Vec<&str> = p.split_whitespace().collect();
                                let out = words.iter().any(|w| *w == "out" || *w == "inout");
                                let rest: Vec<&&str> = words.iter().filter(|w| !QUALIFIERS.contains(w)).collect();
                                let ty = (rest.len() == 2 && !p.contains('[')).then(|| rest[0].to_string());
                                Param { out, ty }
                            })
                            .collect();
                        let overloads = out.entry(name.to_owned()).or_default();
                        if !overloads.contains(&params) {
                            overloads.push(params);
                        }
                        i = close;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    // Built-ins with an `out` parameter (its type is the argument's).
    for f in ["modf", "frexp"] {
        out.entry(f.into()).or_insert_with(|| vec![vec![Param { out: false, ty: None }, Param { out: true, ty: None }]]);
    }
    out
}

/// `a.b[1].z`: the variable and its accessors, when `arg` is only that (at least one accessor).
fn access_chain(arg: &str) -> Option<(&str, Vec<&str>)> {
    let arg = arg.trim();
    let b = arg.as_bytes();
    let mut i = 0;
    while i < b.len() && is_ident_byte(b[i]) {
        i += 1;
    }
    if i == 0 || b[0].is_ascii_digit() {
        return None;
    }
    let base = &arg[..i];
    let mut acc = Vec::new();
    while i < b.len() {
        match b[i] {
            b'.' => {
                let s = i + 1;
                i = s;
                while i < b.len() && is_ident_byte(b[i]) {
                    i += 1;
                }
                if i == s {
                    return None;
                }
                acc.push(&arg[s - 1..i]);
            }
            b'[' => {
                let close = matching(arg, i)?;
                acc.push(&arg[i..=close]);
                i = close + 1;
            }
            _ => return None,
        }
    }
    (!acc.is_empty()).then_some((base, acc))
}

/// The declared type of `name` as seen at `at` (nearest declaration before it), and whether it
/// was declared as an array.
fn declared_type(text: &str, at: usize, name: &str, structs: &BTreeMap<String, BTreeMap<String, String>>) -> Option<(String, bool)> {
    let is_type = |w: &str| is_builtin_type(w) || structs.contains_key(w);
    for p in find_word(&text[..at], name).into_iter().rev() {
        let after = text[p + name.len()..].trim_start();
        let array = after.starts_with('[');
        let before = text[..p].trim_end();
        let w_start = before
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .map_or(0, |q| q + 1);
        let word = &before[w_start..];
        if is_type(word) {
            return Some((word.to_owned(), array));
        }
        if before.ends_with(',') {
            // `vec2 i, f`: the type at the start of the declaration.
            let s = before.rfind([';', '{', '}', '(']).map_or(0, |q| q + 1);
            let first = before[s..]
                .split_whitespace()
                .find(|w| !QUALIFIERS.contains(w) && !w.starts_with("uniform") && *w != "varying")?;
            if is_type(first) {
                return Some((first.to_owned(), array));
            }
        }
    }
    None
}

/// The type of `base` + accessors.
fn chain_type(mut ty: String, mut array: bool, acc: &[&str], structs: &BTreeMap<String, BTreeMap<String, String>>) -> Option<String> {
    for a in acc {
        if a.starts_with('[') {
            if array {
                array = false;
                continue;
            }
            ty = if let Some(n) = ty.strip_prefix("mat") {
                format!("vec{}", n.chars().last()?)
            } else {
                scalar_of(&ty)?.to_owned()
            };
        } else {
            if array {
                return None;
            }
            let field = &a[1..];
            if let Some(m) = structs.get(&ty) {
                ty = m.get(field)?.clone();
            } else {
                let s = scalar_of(&ty)?;
                if !field.bytes().all(|c| b"xyzwrgbastpq".contains(&c)) {
                    return None;
                }
                ty = match field.len() {
                    1 => s.to_owned(),
                    n => format!("{}vec{n}", &ty[..ty.find("vec")?]),
                };
            }
        }
    }
    (!array).then_some(ty)
}

fn scalar_of(ty: &str) -> Option<&'static str> {
    Some(match ty {
        "float" | "int" | "uint" | "bool" | "double" => return None,
        t if t.starts_with("vec") => "float",
        t if t.starts_with("ivec") => "int",
        t if t.starts_with("uvec") => "uint",
        t if t.starts_with("bvec") => "bool",
        t if t.starts_with("dvec") => "double",
        _ => return None,
    })
}

/// The statement holding the call at `at`: (start, end incl. `;`, wrap in braces). `None`
/// for a call in an `if`/`for`/`while` header, a macro or outside a function.
fn statement(text: &str, at: usize, comments: &[(usize, usize)]) -> Option<(usize, usize, bool)> {
    let b = text.as_bytes();
    let line_start = text[..at].rfind('\n').map_or(0, |p| p + 1);
    if text[line_start..].trim_start().starts_with('#') {
        return None;
    }
    // back to the previous `;`, `{` or `}`
    let mut depth = 0i32;
    let mut i = at;
    let mut start = None;
    while i > 0 {
        i -= 1;
        if in_comment(comments, i) {
            continue;
        }
        match b[i] {
            b')' | b']' => depth += 1,
            b'(' | b'[' => {
                depth -= 1;
                if depth < 0 {
                    let w = text[..i].trim_end();
                    if ["if", "for", "while", "switch"].iter().any(|k| {
                        w.ends_with(k) && !w[..w.len() - k.len()].bytes().last().is_some_and(is_ident_byte)
                    }) {
                        return None;
                    }
                }
            }
            b';' | b'{' | b'}' if depth <= 0 => {
                start = Some(i + 1);
                break;
            }
            _ => {}
        }
    }
    let s0 = start?;
    let mut s = s0;
    // after a preprocessor line between the boundary and the call
    for (off, _) in text[s0..at].match_indices('\n') {
        let next = s0 + off + 1;
        if text[next..].trim_start_matches([' ', '\t']).starts_with('#') {
            let end = text[next..].find('\n').map_or(text.len(), |e| next + e);
            if end < at {
                s = end + 1;
            }
        }
    }
    // forward to the `;`
    let mut depth = 0i32;
    let mut j = at;
    let mut end = None;
    while j < b.len() {
        if !in_comment(comments, j) {
            match b[j] {
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                b'{' | b'}' => return None,
                b';' if depth <= 0 => {
                    end = Some(j + 1);
                    break;
                }
                _ => {}
            }
        }
        j += 1;
    }
    let end = end?;
    // peel `else`, `if (..)`, `for (..)`, `while (..)` heads: the call is in their body
    let mut wrap = false;
    loop {
        s += text[s..].len() - text[s..].trim_start().len();
        let rest = &text[s..];
        let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        match word.as_str() {
            "else" => {
                s += 4;
                wrap = true;
            }
            "if" | "for" | "while" => {
                let open = s + rest.find('(')?;
                if text[s + word.len()..open].trim() != "" {
                    break;
                }
                s = matching(text, open)? + 1;
                wrap = true;
            }
            _ => break,
        }
        if s > at {
            return None;
        }
    }
    Some((s, end, wrap))
}

pub fn out_arg_temporary(files: &Files, _: &Eval) -> Edits {
    let srcs = sources(files);
    let sigs = signatures(&srcs);
    let structs = struct_members(&srcs);
    let mut edits = Edits::new();
    for (path, text) in &srcs {
        let comments = comment_ranges(text);
        // statement (start, end, wrap) → (arg start, arg end, temp type)
        let mut by_stmt: BTreeMap<(usize, usize, bool), Vec<(usize, usize, String)>> = BTreeMap::new();
        for (fname, overloads) in &sigs {
            for at in find_word(text, fname) {
                let after = at + fname.len();
                let ws = text[after..].len() - text[after..].trim_start().len();
                let open = after + ws;
                if text.as_bytes().get(open) != Some(&b'(') || text[..at].ends_with('.') {
                    continue;
                }
                // not a definition: a call has an operator or `(`/`,`/`;`/`{` before it, not a type
                let prev = text[..at].trim_end();
                let prev_word_start = prev
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .map_or(0, |p| p + 1);
                let prev_word = &prev[prev_word_start..];
                if !prev_word.is_empty() && prev_word != "return" && prev_word != "else" {
                    continue;
                }
                let Some(close) = matching(text, open) else { continue };
                let args = split_args(text, open, close);
                let fitting: Vec<&Vec<Param>> = overloads.iter().filter(|o| o.len() == args.len()).collect();
                if fitting.is_empty() {
                    continue;
                }
                let Some(stmt) = statement(text, at, &comments) else { continue };
                // arg offsets
                let mut pos = open + 1;
                for (k, arg) in args.iter().enumerate() {
                    let a_start = pos + text[pos..].find(arg.as_str()).unwrap_or(0);
                    let a_end = a_start + arg.len();
                    pos = a_end + 1;
                    let params: Vec<&Param> = fitting.iter().map(|o| &o[k]).collect();
                    if !params.iter().all(|p| p.out) {
                        continue;
                    }
                    let Some((base, acc)) = access_chain(arg) else { continue };
                    let ty = match params[0].ty.clone().filter(|t| params.iter().all(|p| p.ty.as_deref() == Some(t))) {
                        Some(t) => Some(t),
                        None => declared_type(text, at, base, &structs).and_then(|(t, arr)| chain_type(t, arr, &acc, &structs)),
                    };
                    let Some(ty) = ty else { continue };
                    let lead = arg.len() - arg.trim_start().len();
                    let trail = arg.trim_end().len();
                    by_stmt.entry(stmt).or_default().push((a_start + lead, a_start + trail, ty));
                }
            }
        }
        if by_stmt.is_empty() {
            continue;
        }
        let mut out = text.clone();
        let mut n = 0usize;
        let count = by_stmt.values().map(Vec::len).sum::<usize>();
        // statements never overlap (each is between two `;`/braces); go from the end
        let mut stmts: Vec<_> = by_stmt.into_iter().collect();
        stmts.sort_by_key(|((s, _, _), _)| std::cmp::Reverse(*s));
        let first_temp = count;
        let mut next_id = first_temp;
        for ((s, e, wrap), mut args) in stmts {
            args.sort();
            args.dedup_by_key(|a| a.0);
            let mut body = text[s..e].to_owned();
            let mut pre = String::new();
            let mut post = String::new();
            for (a0, a1, ty) in args.iter().rev() {
                next_id -= 1;
                let t = format!("hzp_oa{next_id}");
                let expr = &text[*a0..*a1];
                pre = format!("{ty} {t} = {expr}; {pre}");
                post = format!("{post} {expr} = {t};");
                body.replace_range(a0 - s..a1 - s, &t);
                n += 1;
            }
            let returns = body.trim_start().starts_with("return");
            let post = if returns { String::new() } else { post };
            let new = if wrap {
                format!("{{ {pre}{body}{post} }}")
            } else {
                format!("{pre}{body}{post}")
            };
            out.replace_range(s..e, &new);
        }
        edits.insert(path.clone(), (out, format!("{n} out/inout argument(s) copied through a temporary")));
    }
    edits
}

// ---- pad-outputs ---------------------------------------------------------------------------

/// The `vec4` type and `vec4(x, …, 1.0)` for an output of type `ty`; `None` for types left alone.
fn widen(ty: &str, x: &str) -> Option<(String, String)> {
    let (v4, zero, one) = match ty.chars().next()? {
        'i' => ("ivec4", "0", "1"),
        'u' => ("uvec4", "0u", "1u"),
        _ => ("vec4", "0.0", "1.0"),
    };
    let n = match ty {
        "float" | "int" | "uint" => 1,
        "vec2" | "ivec2" | "uvec2" => 2,
        "vec3" | "ivec3" | "uvec3" => 3,
        _ => return None,
    };
    let fill: Vec<&str> = std::iter::repeat_n(zero, 3 - n).chain([one]).collect();
    Some((v4.to_owned(), format!("{v4}({x}, {})", fill.join(", "))))
}

/// Fragment outputs narrower than four components become private globals; a new `vec4`
/// output at the same location gets them padded (alpha 1) when the pack's `main` returns.
/// (wgpu refuses a `vec3` output into an RGBA buffer; GL takes it.)
pub fn pad_outputs(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    let srcs = sources(files);
    let fragment_only = included_only_by_fsh(&srcs);
    for (path, text) in srcs {
        if path.ends_with(".vsh") || path.ends_with(".gsh") || path.ends_with(".csh") {
            continue;
        }
        let comments = comment_ranges(&text);
        // (decl line start, decl end incl. `;`, qualifiers before `out`, type, name)
        let mut decls = Vec::new();
        for at in find_word(&text, "out") {
            let line_start = text[..at].rfind('\n').map_or(0, |p| p + 1);
            let Some(semi) = text[at..].find(';').map(|i| at + i) else { continue };
            if text[line_start..semi].contains('\n') {
                continue;
            }
            let words: Vec<&str> = text[at + 3..semi].split_whitespace().collect();
            let [ty, name] = words[..] else { continue };
            if !name.bytes().all(is_ident_byte) || widen(ty, name).is_none() {
                continue;
            }
            let quals = text[line_start..at].trim();
            let layout = quals.starts_with("layout") && quals.contains("location") && quals.ends_with(')');
            if !layout && !quals.is_empty() {
                continue;
            }
            if brace_depth_at(&text, &comments, line_start) != 0 {
                continue;
            }
            let line = text[..at].matches('\n').count() + 1;
            let g = guards_at(&text, line);
            if g.iter().any(|m| m.contains("VERTEX") || m.contains("VSH") || m.contains("GEOMETRY") || m.contains("GSH")) {
                continue;
            }
            // a plain `out` is a fragment output only in fragment code
            let fragment = path.ends_with(".fsh")
                || g.iter().any(|m| m.contains("FRAG") || m.contains("FSH"))
                || fragment_only.contains(&path);
            if !layout && !fragment {
                continue;
            }
            decls.push((line_start, semi + 1, quals.to_owned(), ty.to_owned(), name.to_owned()));
        }
        if decls.is_empty() {
            continue;
        }
        // the first `void main()` defined after the last declaration
        let last = decls.iter().map(|d| d.1).max().unwrap();
        let Some(main_at) = find_word(&text, "main").into_iter().find(|&p| {
            p > last && text[..p].trim_end().ends_with("void") && text[p + 4..].trim_start().starts_with('(')
        }) else {
            continue;
        };
        let Some(open) = text[main_at..].find('{').map(|i| main_at + i) else { continue };
        let params = text[main_at + 4..open].trim();
        if params != "()" && params != "(void)" {
            continue;
        }
        let Some(close) = matching(&text, open) else { continue };
        let mut out = text.clone();
        // from the end: the wrapper after main, main's name, then the declarations
        let mut names: Vec<&str> = Vec::new();
        let mut wrapper = String::from("\n// pad-outputs (horizon-shaderport)\nvoid main() {\n    hzp_pad_main();\n");
        for d in &decls {
            if !names.contains(&d.4.as_str()) {
                names.push(&d.4);
                wrapper += &format!("#ifdef HZP_PAD_{0}\n    HZP_PAD_{0}\n#endif\n", d.4);
            }
        }
        wrapper += "}\n";
        out.insert_str(close + 1, &wrapper);
        out.replace_range(main_at..main_at + 4, "hzp_pad_main");
        for (s, e, quals, ty, name) in decls.iter().rev() {
            let (v4, padded) = widen(ty, name).unwrap();
            let indent = &text[*s..*s + (text[*s..].len() - text[*s..].trim_start().len())];
            let sep = if quals.is_empty() { "" } else { " " };
            let new = format!(
                "{indent}{ty} {name}; {quals}{sep}out {v4} hzp_pad_{name};\n#define HZP_PAD_{name} hzp_pad_{name} = {padded};"
            );
            out.replace_range(*s..*e, &new);
        }
        edits.insert(path, (out, format!("{} output(s) padded to 4 components", decls.len())));
    }
    edits
}

fn brace_depth_at(text: &str, comments: &[(usize, usize)], at: usize) -> i32 {
    text[..at]
        .char_indices()
        .filter(|(i, _)| !in_comment(comments, *i))
        .map(|(_, c)| match c {
            '{' => 1,
            '}' => -1,
            _ => 0,
        })
        .sum()
}

/// Include files that only `.fsh` stage files reach (through any chain of `#include`s).
fn included_only_by_fsh(srcs: &[(String, String)]) -> std::collections::BTreeSet<String> {
    use std::collections::{BTreeMap, BTreeSet};
    let known: BTreeSet<&str> = srcs.iter().map(|(p, _)| p.as_str()).collect();
    let mut includes: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (path, text) in srcs {
        let dir = path.rsplit_once('/').map_or("", |d| d.0);
        for line in text.lines() {
            let t = line.trim_start();
            let Some(rest) = t.strip_prefix('#') else { continue };
            let Some(rest) = rest.trim_start().strip_prefix("include") else { continue };
            let Some(name) = rest.trim().strip_prefix('"').and_then(|r| r.split('"').next()) else { continue };
            let joined = match name.strip_prefix('/') {
                Some(abs) => abs.to_owned(),
                None if dir.is_empty() => name.to_owned(),
                None => format!("{dir}/{name}"),
            };
            let mut parts: Vec<&str> = Vec::new();
            for c in joined.split('/') {
                match c {
                    "" | "." => {}
                    ".." => {
                        parts.pop();
                    }
                    c => parts.push(c),
                }
            }
            let target = parts.join("/");
            if known.contains(target.as_str()) {
                includes.entry(path.as_str()).or_default().push(target);
            }
        }
    }
    // stage kinds reaching each file
    let mut reached: BTreeMap<String, BTreeSet<&str>> = BTreeMap::new();
    for (path, _) in srcs {
        let kind = match path.rsplit_once('.').map(|e| e.1) {
            Some(k @ ("vsh" | "fsh" | "gsh" | "csh")) => k,
            _ => continue,
        };
        let mut stack = vec![path.clone()];
        let mut seen = BTreeSet::new();
        while let Some(f) = stack.pop() {
            if !seen.insert(f.clone()) {
                continue;
            }
            reached.entry(f.clone()).or_default().insert(kind);
            if let Some(next) = includes.get(f.as_str()) {
                stack.extend(next.iter().cloned());
            }
        }
    }
    reached
        .into_iter()
        .filter(|(p, k)| !p.ends_with(".fsh") && k.len() == 1 && k.contains("fsh"))
        .map(|(p, _)| p)
        .collect()
}

// ---- drop-unbuffered-outputs ---------------------------------------------------------------

/// Programs the GPU check found with outputs no buffer takes (`gpu.rs`): their stage file gets
/// `#define HZP_NO_OUT_<n>`, and every `layout(location = n) out T x;` becomes the plain global
/// `T x;` under it. GL and Iris drop such writes; Horizon crashes on them.
pub fn drop_unbuffered_outputs(files: &Files, best: &Eval) -> Edits {
    use std::collections::BTreeSet;
    let mut stages: BTreeMap<String, BTreeSet<u32>> = BTreeMap::new();
    for d in &best.dims {
        for s in &d.skipped {
            for p in &s.problems {
                let Some(rest) = p.message.strip_prefix("GPU draw: outputs without a buffer: ") else { continue };
                let list = rest.split(' ').next().unwrap_or("");
                let locs: BTreeSet<u32> = list.split(',').filter_map(|n| n.parse().ok()).collect();
                let path = format!("{}.fsh", s.program);
                if files.text(&path).is_some() && !locs.is_empty() {
                    stages.entry(path).or_default().extend(locs);
                }
            }
        }
    }
    if stages.is_empty() {
        return Edits::new();
    }
    let all: BTreeSet<u32> = stages.values().flatten().copied().collect();
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        if path.ends_with(".vsh") || path.ends_with(".gsh") || path.ends_with(".csh") {
            continue;
        }
        let comments = comment_ranges(&text);
        let mut wraps = Vec::new();
        for at in find_word(&text, "out") {
            let Some(semi) = text[at..].find(';').map(|i| at + i) else { continue };
            let words: Vec<&str> = text[at + 3..semi].split_whitespace().collect();
            let [ty, name] = words[..] else { continue };
            if !name.bytes().all(is_ident_byte) || text[..semi].ends_with(']') {
                continue;
            }
            let before = text[..at].trim_end();
            if !before.ends_with(')') {
                continue;
            }
            let close = before.len() - 1;
            // back to the `(` of `layout(`
            let mut depth = 0;
            let mut open = None;
            for (i, c) in text[..=close].char_indices().rev() {
                match c {
                    ')' => depth += 1,
                    '(' => {
                        depth -= 1;
                        if depth == 0 {
                            open = Some(i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(open) = open else { continue };
            let head = text[..open].trim_end();
            if !head.ends_with("layout") {
                continue;
            }
            let start = head.len() - "layout".len();
            if start > 0 && is_ident_byte(text.as_bytes()[start - 1]) {
                continue;
            }
            let inner: String = text[open + 1..close].split_whitespace().collect();
            let Some(loc) = inner
                .split(',')
                .find_map(|kv| kv.strip_prefix("location=")?.parse::<u32>().ok())
            else {
                continue;
            };
            if !all.contains(&loc) || brace_depth_at(&text, &comments, start) != 0 || in_comment(&comments, start) {
                continue;
            }
            wraps.push((start, semi + 1, loc, ty.to_owned(), name.to_owned()));
        }
        let mut out = text.clone();
        for (s, e, loc, ty, name) in wraps.iter().rev() {
            let orig = &text[*s..*e];
            out.replace_range(
                *s..*e,
                &format!("\n#ifdef HZP_NO_OUT_{loc}\n{ty} {name};\n#else\n{orig}\n#endif\n"),
            );
        }
        let mut detail = Vec::new();
        if !wraps.is_empty() {
            detail.push(format!("{} output declaration(s) made switchable", wraps.len()));
        }
        if let Some(locs) = stages.get(&path) {
            let defs: String = locs.iter().map(|l| format!("#define HZP_NO_OUT_{l}\n")).collect();
            out = crate::shaderport::fix::insert_decl(&out, &defs);
            detail.push(format!("output(s) {} dropped here", locs.iter().map(u32::to_string).collect::<Vec<_>>().join(",")));
        }
        if out != text {
            edits.insert(path, (out, detail.join("; ")));
        }
    }
    edits
}
