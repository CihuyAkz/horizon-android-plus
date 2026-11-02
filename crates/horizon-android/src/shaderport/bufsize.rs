//! Pass `buffer-size`. Horizon ignores `size.buffer.<name> = <x> <y>` (nightly 7aa0e28): every
//! buffer is screen-sized. A pass that writes a buffer scaled down in Iris then covers the whole
//! screen, so its `gl_FragCoord` and its texture coordinates no longer agree (Photon's clouds:
//! `deferred1` renders a quarter-size buffer, reads depth through its screen coordinate, and in
//! Horizon finds terrain everywhere: no cloud is drawn).
//!
//! For a buffer with a relative scale below 1 that the pack only ever reads with `texelFetch` and
//! `textureSize` (so a lookup never depends on the buffer's size):
//!
//! - each pass that writes it draws into the matching corner of the screen-sized buffer, as Iris
//!   draws the whole of the small one (its vertex `main` runs, then the position is scaled);
//! - `textureSize` of it returns the scaled size.
//!
//! The pack's option defaults decide which `size.buffer` lines count (Photon picks its cloud
//! scale with `#if CLOUDS_TEMPORAL_UPSCALING == 4`). Buffers read with `texture()` are left alone.

use std::collections::{BTreeMap, BTreeSet};

use horizon_shaders::{Group, Macros, Properties, dimension_folder, load_post};

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Change, Edits, sources};
use crate::shaderport::glsl::{after_version, comment_ranges, find_word, in_comment, is_ident_byte, matching};

const MARK: &str = "// horizon-shaderport buffer-size";

/// The standard macros plus the pack's option defaults (`#define NAME value // [...]`): the first
/// `#define` of a name in the pack's sources counts.
pub(crate) fn option_macros(files: &Files) -> Macros {
    let mut macros = Macros::standard();
    let mut seen = BTreeSet::new();
    for (_, text) in sources(files) {
        for line in text.lines() {
            let Some(rest) = line.trim_start().strip_prefix('#') else { continue };
            let Some(rest) = rest.trim_start().strip_prefix("define") else { continue };
            if !rest.starts_with(char::is_whitespace) {
                continue;
            }
            let mut parts = rest.split_whitespace();
            let Some(name) = parts.next() else { continue };
            if !name.bytes().all(is_ident_byte) || name.as_bytes()[0].is_ascii_digit() || !seen.insert(name.to_owned()) {
                continue;
            }
            if macros.is_defined(name) {
                continue;
            }
            let value = parts.next().filter(|v| !v.starts_with("//")).unwrap_or("");
            macros.define(name, value);
        }
    }
    macros
}

fn buffer_names(n: u32) -> Vec<String> {
    let mut names = vec![format!("colortex{n}")];
    let alias = match n {
        0 => Some("gcolor"),
        1 => Some("gdepth"),
        2 => Some("gnormal"),
        3 => Some("composite"),
        4 => Some("gaux1"),
        5 => Some("gaux2"),
        6 => Some("gaux3"),
        7 => Some("gaux4"),
        _ => None,
    };
    names.extend(alias.map(str::to_owned));
    names
}

/// The pack's functions that take a `sampler2D` first: name → the types of their second parameter.
fn helper_sigs(srcs: &[(String, String)]) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (_, text) in srcs {
        for at in find_word(text, "sampler2D") {
            let mut before = text[..at].trim_end();
            if let Some(b) = before.strip_suffix("in").or_else(|| before.strip_suffix("const")) {
                if b.ends_with(|c: char| c == '(' || c.is_whitespace()) {
                    before = b.trim_end();
                }
            }
            let Some(b) = before.strip_suffix('(') else { continue };
            let b = b.trim_end();
            let start = b.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(0, |i| i + 1);
            let name = &b[start..];
            if name.is_empty() || !b[..start].trim_end().ends_with(|c: char| c.is_ascii_alphanumeric()) {
                continue; // a call, not a definition (no return type before the name)
            }
            let rest = &text[at + 9..];
            let (Some(comma), Some(close)) = (rest.find(','), rest.find(')')) else { continue };
            if comma > close {
                continue;
            }
            let mut words = rest[comma + 1..].split_whitespace();
            let mut ty = words.next().unwrap_or("");
            if matches!(ty, "in" | "const") {
                ty = words.next().unwrap_or("");
            }
            out.entry(name.to_owned()).or_default().insert(ty.to_owned());
        }
    }
    out
}

/// Lookups whose second argument is a texture coordinate.
const UV_CALLS: &[&str] = &[
    "texture", "texture2D", "textureLod", "texture2DLod", "textureGrad", "textureOffset", "textureLodOffset",
    "textureGather", "textureGatherOffset", "hz_gather", "hz_gather_c", "hz_gather_o", "hz_gather_oc",
    "hz_gather_os", "hz_gather_osc",
];

/// Byte range of the top-level argument `k` of the call whose `(` is at `open`.
fn arg_range(text: &str, open: usize, close: usize, k: usize) -> Option<(usize, usize)> {
    let mut depth = 0i32;
    let mut idx = 0;
    let mut start = open + 1;
    for (i, ch) in text[open + 1..close].char_indices() {
        let at = open + 1 + i;
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                if idx == k {
                    return Some((start, at));
                }
                idx += 1;
                start = at + 1;
            }
            _ => {}
        }
    }
    (idx == k).then_some((start, close))
}

/// Insertions (position, text) that make every read of buffer `n` see Iris's small buffer, or
/// None when a read is of a kind not understood.
fn plan_reads(
    srcs: &[(String, String)],
    n: u32,
    s: (f32, f32),
    sigs: &BTreeMap<String, BTreeSet<String>>,
) -> Option<Vec<(String, usize, String)>> {
    let scale = format!("vec2({}, {})", num(s.0), num(s.1));
    let mut out = Vec::new();
    for (path, text) in srcs {
        let comments = comment_ranges(text);
        for name in buffer_names(n) {
            // A file where the name is a stand-in (a 3D texture), not this buffer.
            if text.contains(&format!("const hz_tex3d {name} ")) {
                continue;
            }
            for at in find_word(text, &name) {
                if in_comment(&comments, at) {
                    continue;
                }
                let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                if text[line_start..at].trim().ends_with("uniform sampler2D") {
                    continue;
                }
                let before = text[..at].trim_end();
                let Some(call) = before.strip_suffix('(') else { return None };
                let open = before.len() - 1;
                let call = call.trim_end();
                let fstart = call.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(0, |i| i + 1);
                let func = &call[fstart..];
                let close = matching(text, open)?;
                let (a0s, a0e) = arg_range(text, open, close, 0)?;
                if text[a0s..a0e].trim() != name {
                    return None;
                }
                match func {
                    "texelFetch" => {}
                    "textureSize" => {
                        out.push((path.clone(), fstart, "ivec2(vec2(".to_owned()));
                        out.push((path.clone(), close + 1, format!(") * {scale} + 0.5)")));
                    }
                    f if UV_CALLS.contains(&f) || sigs.get(f).is_some_and(|t| t.len() == 1 && t.contains("vec2")) => {
                        let (s1, e1) = arg_range(text, open, close, 1)?;
                        out.push((path.clone(), s1, "((".to_owned()));
                        out.push((path.clone(), e1, format!(") * {scale})")));
                    }
                    f if sigs.get(f).is_some_and(|t| t.len() == 1 && t.contains("ivec2")) => {}
                    _ => return None,
                }
            }
        }
    }
    Some(out)
}

/// `size.buffer.colortexN = x y` with x, y relative (a `.`) and below 1.
fn scales(properties: &Properties) -> BTreeMap<u32, (f32, f32)> {
    let mut out = BTreeMap::new();
    for e in &properties.entries {
        let Some(name) = e.key.strip_prefix("size.buffer.") else { continue };
        let Some(n) = horizon_shaders::buffer_index(name) else { continue };
        let parts: Vec<&str> = e.value.split_whitespace().collect();
        if parts.len() != 2 || !parts.iter().all(|p| p.contains('.')) {
            continue;
        }
        let (Ok(x), Ok(y)) = (parts[0].parse::<f32>(), parts[1].parse::<f32>()) else { continue };
        if x > 0.0 && y > 0.0 && (x < 0.999 || y < 0.999) && x <= 1.0 && y <= 1.0 {
            out.insert(n as u32, (x, y));
        }
    }
    out
}

fn num(x: f32) -> String {
    crate::shaderport::tex3d::num(x)
}

fn wrapper(s: (f32, f32)) -> String {
    format!(
        "\n{MARK}: Horizon's buffer is screen-sized; draw into the corner Iris's {}x{} buffer would be\n\
         void main() {{\n    hz_bs_vmain();\n    vec2 hz_bs_n = gl_Position.xy / gl_Position.w;\n    \
         gl_Position.xy = ((hz_bs_n * 0.5 + 0.5) * vec2({}, {}) * 2.0 - 1.0) * gl_Position.w;\n}}\n",
        num(s.0), num(s.1), num(s.0), num(s.1)
    )
}

/// The vertex stage file with its `main` run first and the position scaled.
fn scale_vertex(text: &str, s: (f32, f32)) -> Option<String> {
    if text.contains(MARK) {
        return None;
    }
    let comments = comment_ranges(text);
    let last_main = find_word(text, "main").into_iter().filter(|&at| {
        !in_comment(&comments, at)
            && text[..at].trim_end().ends_with("void")
            && text[at + 4..].trim_start().starts_with('(')
    }).last();
    if let Some(at) = last_main {
        let mut out = text.to_owned();
        out.replace_range(at..at + 4, "hz_bs_vmain");
        out.push_str(&wrapper(s));
        return Some(out);
    }
    if text.contains("#define main") {
        return None;
    }
    let at = after_version(text)?;
    Some(format!("{}#define main hz_bs_vmain\n{}\n#undef main{}", &text[..at], &text[at..], wrapper(s)))
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let Ok(pack) = files.pack() else { return };
    let macros = option_macros(files);
    let ptext = files.text("shaders.properties").unwrap_or_default();
    let properties = Properties::parse(&ptext, &macros);
    let mut scaled = scales(&properties);
    if scaled.is_empty() {
        return;
    }
    let srcs = sources(files);
    let trace = std::env::var_os("HZ_TRACE").is_some();
    let sigs = helper_sigs(&srcs);
    let mut reads: BTreeMap<u32, Vec<(String, usize, String)>> = BTreeMap::new();
    scaled.retain(|n, s| match plan_reads(&srcs, *n, *s, &sigs) {
        Some(r) => {
            reads.insert(*n, r);
            true
        }
        None => {
            if trace {
                eprintln!("  [trace] buffer-size: colortex{n} is read in a way not understood; left alone");
            }
            false
        }
    });
    // Writers: (vertex stage file, the scale), per post program.
    let inputs = crate::shaderport::load::inputs();
    let mut writers: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    let plain = Macros::standard();
    for id in [0, -1, 1] {
        let folder = dimension_folder(&pack, id);
        let Ok(post) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_post(&pack, &folder, &properties, &plain, &inputs)
        })) else {
            continue;
        };
        for p in &post.programs {
            if !matches!(p.group, Group::Prepare | Group::Deferred | Group::Composite) {
                continue;
            }
            if !p.buffers.iter().any(|b| scaled.contains_key(b)) {
                continue;
            }
            let stem = [format!("{folder}/{}", p.name), p.name.clone()]
                .into_iter()
                .find(|s| !s.starts_with('/') && files.text(&format!("{s}.fsh")).is_some());
            let Some(stem) = stem else { continue };
            writers.insert(format!("{stem}.vsh"), p.buffers.clone());
        }
    }
    // A writer that also writes a buffer of another size (or a full-size one) cannot be scaled:
    // its buffers are left alone.
    loop {
        let mut bad = BTreeSet::new();
        for (vsh, bufs) in &writers {
            let sizes: BTreeSet<String> = bufs.iter().map(|b| format!("{:?}", scaled.get(b))).collect();
            if sizes.len() != 1 || files.text(vsh).is_none() {
                bad.extend(bufs.iter().filter(|b| scaled.contains_key(b)).copied());
            }
        }
        if bad.is_empty() {
            break;
        }
        if trace {
            eprintln!("  [trace] buffer-size: not scaled (mixed writers): {bad:?}");
        }
        scaled.retain(|n, _| !bad.contains(n));
        writers.retain(|_, bufs| bufs.iter().any(|b| scaled.contains_key(b)));
    }
    if scaled.is_empty() || writers.is_empty() {
        return;
    }
    // Reads first (insertions in the original text), then the writers' vertex files.
    let mut texts: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut by_file: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
    for (n, r) in &reads {
        if scaled.contains_key(n) {
            for (path, at, ins) in r {
                by_file.entry(path.clone()).or_default().push((*at, ins.clone()));
            }
        }
    }
    for (path, mut ins) in by_file {
        let Some(text) = files.text(&path) else { continue };
        ins.sort_by(|a, b| b.0.cmp(&a.0));
        let mut out = text;
        for (at, t) in ins {
            out.insert_str(at, &t);
        }
        texts.insert(path, (out, "reads scaled".into()));
    }
    for (vsh, bufs) in &writers {
        let s = scaled[&bufs[0]];
        let text = texts.get(vsh).map(|t| t.0.clone()).or_else(|| files.text(vsh)).unwrap_or_default();
        match scale_vertex(&text, s) {
            Some(out) => {
                texts.insert(vsh.clone(), (out, format!("writes buffer(s) {bufs:?} at {} {}", num(s.0), num(s.1))));
            }
            None => {
                if trace {
                    eprintln!("  [trace] buffer-size {vsh}: no main to wrap; nothing scaled");
                }
                return;
            }
        }
    }
    let edits: Edits = texts;
    let Ok(best) = crate::shaderport::load::evaluate(files) else { return };
    let list: Vec<String> = scaled.iter().map(|(n, s)| format!("colortex{n} {} {}", num(s.0), num(s.1))).collect();
    // All or nothing: a writer scaled without its readers (or the reverse) is worse than neither.
    let kept = crate::shaderport::pbr::keep(files, &best, edits, 5);
    println!("  buffer-size: {} file(s) kept ({})", kept.len(), list.join(", "));
    log.push(Change {
        rule: "buffer-size",
        files: kept.into_iter().collect(),
        outcome: format!(
            "Horizon ignores `size.buffer`: passes writing {} draw into the matching corner and reads of them use the small buffer's coordinates",
            list.join(", ")
        ),
    });
}
