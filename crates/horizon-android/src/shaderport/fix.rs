//! Rewrites for what Horizon's loader skips. Every rule is a text change that keeps the GLSL's
//! meaning; each is applied to the whole pack, the pack is loaded again, and the rule is kept
//! only if no program that loaded before stops loading. A rule that breaks something is tried
//! again on halves of the files it touched, so one bad file does not cost the rest.

use std::collections::{BTreeMap, BTreeSet};

use horizon_shaders::{Macros, preprocess};

use crate::shaderport::files::Files;
use crate::shaderport::glsl::{
    after_version, comment_ranges, find_word, in_comment, is_ident_byte, matching, split_args,
};
use crate::shaderport::load::{Eval, evaluate};

pub struct Change {
    pub rule: &'static str,
    pub files: Vec<String>,
    pub outcome: String,
}

/// New text by `shaders/` path, with what changed in it.
pub(crate) type Edits = BTreeMap<String, (String, String)>;

struct Rule {
    name: &'static str,
    /// Why Horizon needs it, for the report.
    why: &'static str,
    apply: fn(&Files, &Eval) -> Edits,
}

const RULES: &[Rule] = &[
    Rule {
        name: "join-macro-calls",
        why: "Horizon's preprocessor expands a function-like macro only when its call is on one line; calls over several lines are joined (line count kept)",
        apply: join_macro_calls,
    },
    Rule {
        name: "const-in-params",
        why: "naga's GLSL reader rejects `const in` parameters; `const` is dropped (it only forbids writes)",
        apply: const_in_params,
    },
    Rule {
        name: "rename-sampler",
        why: "`sampler` is a type in the Vulkan GLSL Horizon translates to; identifiers named so are renamed",
        apply: rename_sampler,
    },
    Rule {
        name: "fog-frag-coord",
        why: "`gl_FogFragCoord` (a compatibility varying) is unknown to Horizon; it becomes a varying of the pack's own",
        apply: fog_frag_coord,
    },
    Rule {
        name: "infinite-literal",
        why: "naga refuses constant `x / 0.0` (infinity); it becomes ±3.0e38",
        apply: infinite_literal,
    },
    Rule {
        name: "texture-gather",
        why: "naga's GLSL reader has no `textureGather*`; calls become four `texelFetch`es (same texels, same order)",
        apply: texture_gather,
    },
    Rule {
        name: "vector-from-bool",
        why: "naga cannot build a float vector from a comparison; `vec3(a < b)` becomes `vec3(float(a < b))`",
        apply: vector_from_bool,
    },
    Rule {
        name: "const-mix",
        why: "naga cannot evaluate `mix()` in a constant; in `const` declarations it is written out as `a + (b - a) * t`",
        apply: const_mix,
    },
    Rule {
        name: "vertex-texture-lod",
        why: "a vertex shader has no derivatives, so naga refuses `texture()` there; in `.vsh` files it samples mip 0 (`textureLod(s, uv, 0.0)`), as GL does",
        apply: vertex_texture_lod,
    },
    Rule {
        name: "code-after-return",
        why: "naga fails on statements after an unconditional `return` in the same block; such a `return` becomes `if (true) return`",
        apply: code_after_return,
    },
    Rule {
        name: "legacy-functions",
        why: "`texelFetch2D`, `textureSize2D` and the like (EXT_gpu_shader4 names) are not renamed by Horizon; they become `texelFetch` / `textureSize`",
        apply: legacy_functions,
    },
    Rule {
        name: "declare-missing",
        why: "a variable declared only under a macro Iris defines and Horizon does not (`MC_NORMAL_MAP`...) is declared again where it is used, as the pack declares it elsewhere (a varying stays a varying)",
        apply: declare_missing,
    },
    Rule {
        name: "struct-varyings",
        why: "naga cannot pass a struct between stages (`flat out FogParams fog;`); each member becomes its own varying, copied out at the end of the vertex `main` and back into the struct at the start of the fragment `main`",
        apply: struct_varyings,
    },
    Rule {
        name: "struct-array-ctor",
        why: "naga cannot build a struct with an array member from its constructor (`S(1.0, float[3](...))`); inside functions the declaration becomes member-by-member assignments",
        apply: struct_array_ctor,
    },
    Rule {
        name: "missing-return",
        why: "a function that can reach its end without returning a value (GL drivers return garbage, usually 0; naga refuses it) gets `return TYPE(0);` at its end",
        apply: missing_return,
    },
    Rule {
        name: "images-off",
        why: "Horizon has no images or storage buffers yet; the pack's own switch around that code (`#define VX_SUPPORT`...) is turned off, so it takes the path it takes on GPUs without them (a FEATURE IS LOST: usually voxel/colored lighting)",
        apply: images_off,
    },
    Rule {
        name: "int-vector-modulo",
        why: "naga has no `ivecN % int`; the scalar is made a vector (`ivec2(x) % ivec2(n)`)",
        apply: int_vector_modulo,
    },
    Rule {
        name: "out-arg-temporary",
        why: "naga's SPIR-V writer (the game's GPU compile) panics on an `out`/`inout` argument that is part of a variable (`f(v.z)`); it is copied through a temporary (`T t = v.z; f(t); v.z = t;`)",
        apply: crate::shaderport::gpu_fix::out_arg_temporary,
    },
    Rule {
        name: "pad-outputs",
        why: "wgpu refuses a fragment output with fewer components than its buffer (`vec3` into RGBA); such outputs are written through a `vec4` output (alpha 1) after the pack's `main`",
        apply: crate::shaderport::gpu_fix::pad_outputs,
    },
    Rule {
        name: "drop-unbuffered-outputs",
        why: "Horizon crashes when a gbuffers/shadow program has an output past its `RENDERTARGETS` (GL drops those writes); for the programs where that is so with the pack's default options, such outputs become plain variables",
        apply: crate::shaderport::gpu_fix::drop_unbuffered_outputs,
    },
    Rule {
        name: "stand-in-3d",
        why: "the game binds every pack texture as 2D and loads no custom texture files, so a program sampling a `sampler3D` is refused; each becomes a stand-in built from the pack's data file (a shrunk table for smooth data, matched noise for noise volumes; APPROXIMATE)",
        apply: crate::shaderport::tex3d::stand_in_3d,
    },
    Rule {
        name: "compute-to-fragment",
        why: "Horizon runs no compute programs; a small one that writes one `colorimgN` becomes the fragment program of the same name, run on a corner of the screen as large as its work, each pixel keeping the store aimed at it",
        apply: crate::shaderport::compute::compute_to_fragment,
    },
];

pub fn fix(files: &mut Files, before: &Eval) -> Vec<Change> {
    let mut log = Vec::new();
    if before.score() == 0 {
        return log; // Horizon loads it all: leave the pack as it is.
    }
    let mut best = before.clone();
    // Twice: a rule can unblock code another rule then reaches.
    for round in 0..2 {
        let mut progressed = false;
        for rule in RULES {
            let edits = (rule.apply)(files, &best);
            let edits: Edits = edits
                .into_iter()
                .filter(|(k, (t, _))| files.text(k).as_deref() != Some(t.as_str()))
                .collect();
            if edits.is_empty() {
                continue;
            }
            let (kept, eval) = try_edits(files, &best, edits, 0);
            if kept.is_empty() {
                if round == 0 {
                    log.push(Change {
                        rule: rule.name,
                        files: Vec::new(),
                        outcome: "not kept: made a loading program fail".into(),
                    });
                }
                continue;
            }
            let eval = eval.expect("kept edits come with their evaluation");
            let gained = best.score() as i64 - eval.score() as i64;
            progressed |= gained > 0;
            log.push(Change {
                rule: rule.name,
                files: kept.keys().cloned().collect(),
                outcome: if gained > 0 {
                    format!("{gained} more program(s) load")
                } else {
                    "kept (no program loads yet because of it alone)".into()
                },
            });
            println!("  {}: {} file(s), {} skipped now", rule.name, kept.len(), eval.skipped_count());
            best = eval;
        }
        if !progressed && round > 0 {
            break;
        }
    }
    log
}

/// Applies the rule `name` without checking it (the `apply` debugging command).
pub fn apply_one(files: &mut Files, eval: &Eval, name: &str) -> Result<usize, String> {
    let rule = RULES.iter().find(|r| r.name == name).ok_or(format!("no rule {name}"))?;
    let edits = (rule.apply)(files, eval);
    for (k, (t, detail)) in &edits {
        println!("  {k}: {detail}");
        files.set_text(k, t.clone());
    }
    Ok(edits.len())
}

/// Applies `edits`; keeps them if nothing that loaded stops loading, else tries halves.
/// Returns the edits kept and the evaluation after them.
fn try_edits(files: &mut Files, best: &Eval, edits: Edits, depth: u32) -> (Edits, Option<Eval>) {
    let saved: Vec<(String, Option<Vec<u8>>)> = edits
        .keys()
        .map(|k| (k.clone(), files.remove(k)))
        .collect();
    for (k, data) in &saved {
        files.restore(k, data.clone());
    }
    for (k, (text, _)) in &edits {
        files.set_text(k, text.clone());
    }
    let result = evaluate(files);
    if std::env::var_os("HZ_TRACE").is_some() {
        let keys: Vec<&String> = edits.keys().collect();
        match &result {
            Ok(e) => eprintln!(
                "  [trace] depth {depth}: {} file(s) {:?}: skipped {} -> {}, regressed {}, signature changed {}",
                keys.len(),
                keys.iter().take(4).collect::<Vec<_>>(),
                best.score(),
                e.score(),
                regressed(best, e),
                e.signature() != best.signature()
            ),
            Err(err) => eprintln!("  [trace] depth {depth}: error {err}"),
        }
    }
    // Only a rule that broke something is worth splitting: one that changed nothing would
    // change nothing in halves either.
    let broke = match &result {
        Ok(e) => regressed(best, e) || e.score() > best.score(),
        Err(_) => true,
    };
    let ok = match result {
        Ok(eval)
            if !regressed(best, &eval)
                && (eval.score() < best.score()
                    || (eval.score() == best.score() && eval.signature() != best.signature())
                    || (eval.score() == best.score() && loaded(&eval) > loaded(best))) =>
        {
            Some(eval)
        }
        _ => None,
    };
    if let Some(eval) = ok {
        return (edits, Some(eval));
    }
    for (k, data) in saved {
        files.restore(&k, data);
    }
    if !broke || edits.len() < 2 || depth >= 4 {
        return (Edits::new(), None);
    }
    let half = edits.len() / 2;
    let mut first = edits;
    let second = first.split_off(first.keys().nth(half).unwrap().clone().as_str());
    let (mut kept, eval1) = try_edits(files, best, first, depth + 1);
    let base = eval1.clone().unwrap_or_else(|| best.clone());
    let (kept2, eval2) = try_edits(files, &base, second, depth + 1);
    kept.extend(kept2);
    (kept, eval2.or(eval1))
}

/// Programs that load, in every dimension (a rule can add one: `compute-to-fragment`).
fn loaded(e: &Eval) -> usize {
    e.dims.iter().map(|d| d.loaded.len()).sum()
}

/// A program that loaded before and now fails, or a loader panic. (A fallback that is no
/// longer loaded because the program it stood in for now loads is not a regression.)
fn regressed(before: &Eval, after: &Eval) -> bool {
    if after.panicked() && !before.panicked() {
        return true;
    }
    before
        .dims
        .iter()
        .zip(&after.dims)
        .any(|(b, a)| a.skipped.iter().any(|s| b.loaded.contains(&s.program)))
}

/// The `shaders/` paths of every stage file, and the text of each.
pub(crate) fn sources(files: &Files) -> Vec<(String, String)> {
    files
        .entries
        .iter()
        .filter_map(|(k, v)| {
            let p = k.strip_prefix("shaders/")?;
            let ext = p.rsplit_once('.')?.1;
            ["vsh", "fsh", "gsh", "csh", "glsl", "inc", "h"]
                .contains(&ext)
                .then(|| (p.to_owned(), String::from_utf8_lossy(v).into_owned()))
        })
        .collect()
}

pub(crate) fn is_stage(path: &str) -> bool {
    path.ends_with(".vsh") || path.ends_with(".fsh")
}

/// Stage files whose preprocessed code (includes and macros resolved) contains `needle`.
pub(crate) fn stages_using(files: &Files, needle: &str) -> BTreeSet<String> {
    let Ok(pack) = files.pack() else {
        return BTreeSet::new();
    };
    let macros = Macros::standard();
    sources(files)
        .into_iter()
        .filter(|(p, _)| is_stage(p))
        .filter(|(p, _)| preprocess(&pack, p, &macros).code.contains(needle))
        .map(|(p, _)| p)
        .collect()
}

/// Inserts `decl` (whole lines) after the stage file's `#version`, or at its top.
pub(crate) fn insert_decl(text: &str, decl: &str) -> String {
    let at = after_version(text).unwrap_or(0);
    format!("{}{}{}", &text[..at], decl, &text[at..])
}

// ---- rules -------------------------------------------------------------------------------

fn join_macro_calls(files: &Files, _: &Eval) -> Edits {
    let srcs = sources(files);
    let mut names = BTreeSet::new();
    for (_, text) in &srcs {
        for line in text.lines() {
            let t = line.trim_start();
            if let Some(rest) = t.strip_prefix('#') {
                let rest = rest.trim_start();
                if let Some(def) = rest.strip_prefix("define") {
                    let def = def.trim_start();
                    let name: String = def.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    if !name.is_empty() && def[name.len()..].starts_with('(') {
                        names.insert(name);
                    }
                }
            }
        }
    }
    let mut edits = Edits::new();
    for (path, text) in srcs {
        let mut out = String::with_capacity(text.len());
        let mut pos = 0;
        let mut joined = 0;
        let comments = comment_ranges(&text);
        let mut calls: Vec<(usize, usize, usize)> = Vec::new();
        for name in &names {
            for at in find_word(&text, name) {
                let after = at + name.len();
                let open = after + text[after..].len() - text[after..].trim_start_matches([' ', '\t']).len();
                if text.as_bytes().get(open) != Some(&b'(') {
                    continue;
                }
                // Not inside a #define (its line, or a continued one).
                let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
                if text[ls..at].trim_start().starts_with('#') || text[..ls].trim_end_matches(['\n', '\r']).ends_with('\\') {
                    continue;
                }
                let Some(close) = matching(&text, open) else { continue };
                if text[at..close].contains('\n') && !in_comment(&comments, at) {
                    calls.push((at, open, close));
                }
            }
        }
        calls.sort();
        for (at, _open, close) in calls {
            if at < pos {
                continue; // inside a call already joined
            }
            let segment = &text[at..=close];
            let newlines = segment.matches('\n').count();
            let mut flat = String::new();
            let seg_comments = comment_ranges(segment);
            for (i, ch) in segment.char_indices() {
                if in_comment(&seg_comments, i) {
                    continue;
                }
                flat.push(if ch == '\n' || ch == '\r' { ' ' } else { ch });
            }
            out.push_str(&text[pos..at]);
            out.push_str(&flat);
            // The rest of the closing line, then the lines the call took, empty.
            let eol = text[close + 1..].find('\n').map_or(text.len(), |n| close + 1 + n);
            out.push_str(&text[close + 1..eol]);
            out.push_str(&"\n".repeat(newlines));
            pos = eol;
            joined += 1;
        }
        if joined > 0 {
            out.push_str(&text[pos..]);
            edits.insert(path, (out, format!("{joined} call(s) joined")));
        }
    }
    edits
}

fn const_in_params(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let mut out = text.clone();
        let mut n = 0;
        for at in find_word(&text, "const").into_iter().rev() {
            let rest = &text[at + 5..];
            let ws = rest.len() - rest.trim_start().len();
            let next = &rest[ws..];
            if next.starts_with("in ") || next.starts_with("in\t") {
                out.replace_range(at..at + 5 + ws, "");
                n += 1;
            }
        }
        if n > 0 {
            edits.insert(path, (out, format!("{n} parameter(s)")));
        }
    }
    edits
}

fn replace_word(text: &str, word: &str, with: &str) -> (String, usize) {
    let hits = find_word(text, word);
    let mut out = text.to_owned();
    for &at in hits.iter().rev() {
        out.replace_range(at..at + word.len(), with);
    }
    (out, hits.len())
}

fn rename_sampler(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let (out, n) = replace_word(&text, "sampler", "hz_sampler_");
        if n > 0 {
            edits.insert(path, (out, format!("{n} use(s)")));
        }
    }
    edits
}

fn fog_frag_coord(files: &Files, _: &Eval) -> Edits {
    let users = stages_using(files, "gl_FogFragCoord");
    if users.is_empty() {
        return Edits::new();
    }
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let (mut out, n) = replace_word(&text, "gl_FogFragCoord", "hz_FogFragCoord");
        if users.contains(&path) {
            let stem = &path[..path.len() - 4];
            let decl = if path.ends_with(".vsh") {
                if users.contains(&format!("{stem}.fsh")) {
                    "out float hz_FogFragCoord;\n"
                } else {
                    "float hz_FogFragCoord;\n"
                }
            } else if users.contains(&format!("{stem}.vsh")) {
                "in float hz_FogFragCoord;\n"
            } else {
                "float hz_FogFragCoord = 0.0;\n"
            };
            out = insert_decl(&out, decl);
        } else if n == 0 {
            continue;
        }
        edits.insert(path, (out, "gl_FogFragCoord → hz_FogFragCoord".into()));
    }
    edits
}

fn infinite_literal(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let comments = comment_ranges(&text);
        let b = text.as_bytes();
        let mut out = text.clone();
        let mut n = 0;
        let mut hits = Vec::new();
        for (slash, _) in text.match_indices('/') {
            if in_comment(&comments, slash)
                || b.get(slash + 1) == Some(&b'/')
                || b.get(slash + 1) == Some(&b'*')
                || (slash > 0 && (b[slash - 1] == b'/' || b[slash - 1] == b'*'))
            {
                continue;
            }
            // Right side: 0.0 (any zeros), optional f.
            let r = slash + 1 + (text[slash + 1..].len() - text[slash + 1..].trim_start().len());
            let zero_len = text[r..]
                .bytes()
                .take_while(|c| *c == b'0' || *c == b'.')
                .count();
            let zero = &text[r..r + zero_len];
            if !zero.contains('.') || !zero.starts_with('0') || zero.len() < 2 {
                continue;
            }
            let mut end = r + zero_len;
            if b.get(end) == Some(&b'f') {
                end += 1;
            }
            if b.get(end).is_some_and(|c| is_ident_byte(*c)) {
                continue;
            }
            // Left side: a number literal.
            let l_end = text[..slash].trim_end().len();
            let l_start = text[..l_end]
                .rfind(|c: char| !(c.is_ascii_digit() || c == '.' || c == 'f' || c == 'e'))
                .map_or(0, |i| i + 1);
            let lit = &text[l_start..l_end];
            if lit.is_empty()
                || !lit.starts_with(|c: char| c.is_ascii_digit() || c == '.')
                || (l_start > 0 && is_ident_byte(b[l_start - 1]))
            {
                continue;
            }
            hits.push((l_start, end));
        }
        for (s, e) in hits.into_iter().rev() {
            out.replace_range(s..e, "3.0e38");
            n += 1;
        }
        if n > 0 {
            edits.insert(path, (out, format!("{n} literal(s)")));
        }
    }
    edits
}

const GATHER_POLYFILL: &str = "\
#define hz_gather_base(s, uv) ivec2(floor((uv) * vec2(textureSize(s, 0)) - 0.5))
#define hz_gather_px(s, p, c) texelFetch(s, clamp(p, ivec2(0), textureSize(s, 0) - 1), 0)[c]
#define hz_gather_at(s, b, c) vec4(hz_gather_px(s, (b) + ivec2(0, 1), c), hz_gather_px(s, (b) + ivec2(1, 1), c), hz_gather_px(s, (b) + ivec2(1, 0), c), hz_gather_px(s, (b), c))
#define hz_gather(s, uv) hz_gather_at(s, hz_gather_base(s, uv), 0)
#define hz_gather_c(s, uv, c) hz_gather_at(s, hz_gather_base(s, uv), c)
#define hz_gather_o(s, uv, o) hz_gather_at(s, hz_gather_base(s, uv) + (o), 0)
#define hz_gather_oc(s, uv, o, c) hz_gather_at(s, hz_gather_base(s, uv) + (o), c)
#define hz_gather_os(s, uv, o) vec4(hz_gather_px(s, hz_gather_base(s, uv) + (o)[0], 0), hz_gather_px(s, hz_gather_base(s, uv) + (o)[1], 0), hz_gather_px(s, hz_gather_base(s, uv) + (o)[2], 0), hz_gather_px(s, hz_gather_base(s, uv) + (o)[3], 0))
#define hz_gather_osc(s, uv, o, c) vec4(hz_gather_px(s, hz_gather_base(s, uv) + (o)[0], c), hz_gather_px(s, hz_gather_base(s, uv) + (o)[1], c), hz_gather_px(s, hz_gather_base(s, uv) + (o)[2], c), hz_gather_px(s, hz_gather_base(s, uv) + (o)[3], c))
";

fn texture_gather(files: &Files, _: &Eval) -> Edits {
    let srcs = sources(files);
    // Shadow samplers compare instead of gathering a component: left alone.
    let mut shadow = BTreeSet::new();
    for (_, text) in &srcs {
        for at in find_word(text, "sampler2DShadow") {
            let rest = text[at + 15..].trim_start();
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            if !name.is_empty() {
                shadow.insert(name);
            }
        }
    }
    let mut edits = Edits::new();
    for (path, text) in &srcs {
        let mut out = text.clone();
        let mut n = 0;
        let mut hits: Vec<(usize, usize, &str)> = Vec::new();
        for (word, two, three, four) in [
            ("textureGather", "hz_gather", "hz_gather_c", ""),
            ("textureGatherOffset", "", "hz_gather_o", "hz_gather_oc"),
            ("textureGatherOffsets", "", "hz_gather_os", "hz_gather_osc"),
        ] {
            for at in find_word(text, word) {
                let open = at + word.len();
                if text.as_bytes().get(open) != Some(&b'(') {
                    continue;
                }
                let Some(close) = matching(text, open) else { continue };
                let args = split_args(text, open, close);
                if args.first().is_some_and(|a| shadow.contains(a.as_str())) {
                    continue;
                }
                let to = match args.len() {
                    2 => two,
                    3 => three,
                    4 => four,
                    _ => "",
                };
                if !to.is_empty() {
                    hits.push((at, at + word.len(), to));
                }
            }
        }
        hits.sort();
        for (s, e, to) in hits.into_iter().rev() {
            out.replace_range(s..e, to);
            n += 1;
        }
        if n > 0 {
            edits.insert(path.clone(), (out, format!("{n} call(s)")));
        }
    }
    if edits.is_empty() {
        return edits;
    }
    // The macros go in every stage file that now reaches a call.
    let mut after = Files {
        name: files.name.clone(),
        entries: files.entries.clone(),
        layout_notes: Vec::new(),
    };
    for (k, (t, _)) in &edits {
        after.set_text(k, t.clone());
    }
    for stage in stages_using(&after, "hz_gather") {
        let text = after.text(&stage).unwrap_or_default();
        let entry = edits
            .entry(stage.clone())
            .or_insert_with(|| (text.clone(), String::new()));
        entry.0 = insert_decl(&entry.0, GATHER_POLYFILL);
        entry.1 = format!("{} polyfill added", entry.1).trim().to_owned();
    }
    edits
}

fn vector_from_bool(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let mut hits = Vec::new();
        for (ctor, scalar) in [
            ("vec2", "float"),
            ("vec3", "float"),
            ("vec4", "float"),
            ("ivec2", "int"),
            ("ivec3", "int"),
            ("ivec4", "int"),
            ("uvec2", "uint"),
            ("uvec3", "uint"),
            ("uvec4", "uint"),
        ] {
            for at in find_word(&text, ctor) {
                let open = at + ctor.len();
                if text.as_bytes().get(open) != Some(&b'(') {
                    continue;
                }
                let Some(close) = matching(&text, open) else { continue };
                let args = split_args(&text, open, close);
                if args.len() != 1 || !top_level_comparison(&args[0]) {
                    continue;
                }
                hits.push((open + 1, close, scalar));
            }
        }
        if hits.is_empty() {
            continue;
        }
        hits.sort();
        // Innermost last-first so offsets stay valid; skip nested overlaps.
        let mut out = text.clone();
        let mut last_start = usize::MAX;
        let n = hits.len();
        for (s, e, scalar) in hits.into_iter().rev() {
            if e > last_start {
                continue;
            }
            out.replace_range(s..e, &format!("{scalar}({})", &text[s..e]));
            last_start = s;
        }
        edits.insert(path, (out, format!("{n} constructor(s)")));
    }
    edits
}

/// `a < b`, `x == y`... at depth 0, and no `?:` (whose value is not a bool).
fn top_level_comparison(arg: &str) -> bool {
    let b = arg.as_bytes();
    let mut depth = 0i32;
    let mut found = false;
    for i in 0..b.len() {
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'?' if depth == 0 => return false,
            b'<' | b'>' if depth == 0 => {
                let next = b.get(i + 1).copied();
                let prev = if i > 0 { b[i - 1] } else { 0 };
                if next != Some(b[i]) && prev != b[i] {
                    found = true;
                }
            }
            b'=' if depth == 0 => {
                if b.get(i + 1) == Some(&b'=') || (i > 0 && b[i - 1] == b'!') {
                    found = true;
                }
            }
            _ => {}
        }
    }
    found
}

fn const_mix(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let mut out = text.clone();
        let mut n = 0;
        // Each `const` declaration (not a `const in` parameter), up to its `;`.
        let consts = find_word(&text, "const");
        for &at in consts.iter().rev() {
            let Some(semi) = out[at..].find(';').map(|i| at + i) else { continue };
            let stmt = &out[at..semi];
            if stmt.contains('{') || !stmt.contains('=') {
                continue;
            }
            let mut s = stmt.to_owned();
            let mut changed = false;
            // Innermost first: the last `mix(` in the text.
            while let Some(m) = find_word(&s, "mix").into_iter().rev().find(|&m| s.as_bytes().get(m + 3) == Some(&b'(')) {
                let open = m + 3;
                let Some(close) = matching(&s, open) else { break };
                let args = split_args(&s, open, close);
                if args.len() != 3 {
                    break;
                }
                let (a, b, t) = (&args[0], &args[1], &args[2]);
                let expanded = format!("(({a}) + (({b}) - ({a})) * ({t}))");
                s.replace_range(m..=close, &expanded);
                changed = true;
            }
            if changed {
                // Keep the declaration's line count.
                let lines = stmt.matches('\n').count();
                let flat: String = s.replace('\n', " ");
                out.replace_range(at..semi, &format!("{flat}{}", "\n".repeat(lines)));
                n += 1;
            }
        }
        if n > 0 {
            edits.insert(path, (out, format!("{n} constant(s)")));
        }
    }
    edits
}

const VERTEX_LOD: &str = "\
#define texture(s, uv) textureLod(s, uv, 0.0)
#define texture2D(s, uv) textureLod(s, uv, 0.0)
#define texture3D(s, uv) textureLod(s, uv, 0.0)
";

fn vertex_texture_lod(files: &Files, _: &Eval) -> Edits {
    let Ok(pack) = files.pack() else {
        return Edits::new();
    };
    let macros = Macros::standard();
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        if !path.ends_with(".vsh") || text.contains("#define texture(s, uv) textureLod") {
            continue;
        }
        let code = preprocess(&pack, &path, &macros).code;
        let uses = ["texture", "texture2D", "texture3D"]
            .iter()
            .any(|w| find_word(&code, w).iter().any(|&at| code[at + w.len()..].trim_start().starts_with('(')));
        if uses {
            edits.insert(path, (insert_decl(&text, VERTEX_LOD), "texture() → textureLod(…, 0.0)".into()));
        }
    }
    edits
}

fn code_after_return(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let comments = comment_ranges(&text);
        let mut hits = Vec::new();
        for at in find_word(&text, "return") {
            let Some(semi) = text[at..].find(';').map(|i| at + i) else { continue };
            // The previous token: `if (...)` / `else` bodies are left alone.
            let before = text[..at].trim_end();
            if before.ends_with(')') || before.ends_with("else") {
                continue;
            }
            // The next token after comments, blank lines and directives.
            let mut i = semi + 1;
            let next = loop {
                let rest = &text[i..];
                let trimmed = rest.trim_start();
                i += rest.len() - trimmed.len();
                if trimmed.is_empty() {
                    break "";
                }
                if in_comment(&comments, i) {
                    i = comments.iter().find(|&&(s, e)| i >= s && i < e).map_or(i + 1, |&(_, e)| e);
                    continue;
                }
                if trimmed.starts_with('#') {
                    // What follows `#else` / `#elif` is the other branch, not code after
                    // this return.
                    let directive = trimmed[1..].trim_start();
                    if directive.starts_with("else") || directive.starts_with("elif") {
                        break "";
                    }
                    i += trimmed.find('\n').unwrap_or(trimmed.len());
                    continue;
                }
                break trimmed;
            };
            if next.is_empty()
                || next.starts_with('}')
                || ["else", "case", "default"].iter().any(|w| {
                    next.starts_with(w) && !next.as_bytes().get(w.len()).is_some_and(|c| is_ident_byte(*c))
                })
            {
                continue;
            }
            hits.push(at);
        }
        if hits.is_empty() {
            continue;
        }
        let mut out = text.clone();
        for &at in hits.iter().rev() {
            out.insert_str(at, "if (true) ");
        }
        edits.insert(path, (out, format!("{} return(s)", hits.len())));
    }
    edits
}

fn legacy_functions(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let mut out = text.clone();
        let mut n = 0;
        for (old, new) in [
            ("texelFetch1D", "texelFetch"),
            ("texelFetch2D", "texelFetch"),
            ("texelFetch3D", "texelFetch"),
            ("textureSize1D", "textureSize"),
            ("textureSize2D", "textureSize"),
            ("textureSize3D", "textureSize"),
        ] {
            let current = out.clone();
            for at in find_word(&current, old).into_iter().rev() {
                let ls = current[..at].rfind('\n').map_or(0, |i| i + 1);
                if current[ls..at].trim_start().starts_with('#') {
                    continue;
                }
                out.replace_range(at..at + old.len(), new);
                n += 1;
            }
        }
        if n > 0 {
            edits.insert(path, (out, format!("{n} call(s)")));
        }
    }
    edits
}

/// A global declaration of `name` in the pack: (`varying`/`uniform`/`""`, type).
fn find_declaration(srcs: &[(String, String)], name: &str) -> Option<(&'static str, String)> {
    for (_, text) in srcs {
        for at in find_word(text, name) {
            let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
            let line_end = text[at..].find('\n').map_or(text.len(), |i| at + i);
            let after = text[at + name.len()..line_end].trim_start();
            if !after.starts_with(';') {
                continue;
            }
            let words: Vec<&str> = text[ls..at].split_whitespace().collect();
            let Some((ty, quals)) = words.split_last() else { continue };
            if !ty.chars().all(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let kind = if quals.iter().any(|q| ["varying", "in", "out"].contains(q)) {
                "varying"
            } else if quals.contains(&"uniform") {
                "uniform"
            } else if quals.iter().all(|q| ["flat", "smooth", "noperspective", "highp", "mediump", "lowp"].contains(q)) {
                ""
            } else {
                continue;
            };
            let flat = if quals.contains(&"flat") { "flat " } else { "" };
            return Some((kind, format!("{flat}{ty}")));
        }
    }
    None
}

fn declare_missing(files: &Files, best: &Eval) -> Edits {
    let mut wanted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for skip in best.unique_skips() {
        for p in &skip.problems {
            if let Some(name) = p.message.strip_prefix("Unknown variable: ") {
                if name.starts_with("gl_") || name.starts_with("hz_") {
                    continue;
                }
                wanted.entry(name.trim().to_owned()).or_default().insert(skip.program.clone());
            }
        }
    }
    if wanted.is_empty() {
        return Edits::new();
    }
    let srcs = sources(files);
    let mut edits = Edits::new();
    for (name, programs) in wanted {
        let Some((kind, ty)) = find_declaration(&srcs, &name) else { continue };
        let users = stages_using(files, &name);
        for program in programs {
            for ext in ["vsh", "fsh"] {
                let stage = format!("{program}.{ext}");
                if !users.contains(&stage) {
                    continue;
                }
                let other = format!("{program}.{}", if ext == "vsh" { "fsh" } else { "vsh" });
                let (flat, base_ty) = match ty.strip_prefix("flat ") {
                    Some(t) => ("flat ", t.to_owned()),
                    None => ("", ty.clone()),
                };
                let decl = match kind {
                    "varying" if users.contains(&other) => {
                        format!("{flat}{} {base_ty} {name};\n", if ext == "vsh" { "out" } else { "in" })
                    }
                    "uniform" => format!("uniform {base_ty} {name};\n"),
                    _ => format!("{base_ty} {name};\n"),
                };
                let text = edits
                    .get(&stage)
                    .map(|e| e.0.clone())
                    .or_else(|| files.text(&stage))
                    .unwrap_or_default();
                if text.is_empty() {
                    continue;
                }
                let detail = format!("declared `{}`", decl.trim());
                edits.insert(stage, (insert_decl(&text, &decl), detail));
            }
        }
    }
    edits
}

/// Struct types of the pack whose members are all plain `type name;` (no arrays, no structs).
fn plain_structs(srcs: &[(String, String)]) -> BTreeMap<String, Vec<(String, String)>> {
    let mut out = BTreeMap::new();
    for (_, text) in srcs {
        for at in find_word(text, "struct") {
            let rest = &text[at + 6..];
            let name: String = rest.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let Some(open) = text[at..].find('{').map(|i| at + i) else { continue };
            if name.is_empty() || text[at + 6..open].trim() != name {
                continue;
            }
            let Some(close) = matching(text, open) else { continue };
            let body: String = {
                let c = comment_ranges(&text[open + 1..close]);
                text[open + 1..close]
                    .char_indices()
                    .filter(|(i, _)| !in_comment(&c, *i))
                    .map(|(_, ch)| ch)
                    .collect()
            };
            let mut members = Vec::new();
            let mut ok = true;
            for decl in body.split(';').map(str::trim).filter(|d| !d.is_empty()) {
                let words: Vec<&str> = decl.split_whitespace().collect();
                if words.len() < 2 || decl.contains('[') || decl.contains('#') {
                    ok = false;
                    break;
                }
                let ty = words[words.len() - 2];
                let builtin = ty.starts_with("vec") || ty.starts_with("ivec") || ty.starts_with("uvec")
                    || ty.starts_with("mat") || ["float", "int", "uint", "bool"].contains(&ty);
                if !builtin || ty == "bool" {
                    ok = false;
                    break;
                }
                // `vec3 a, b;`
                for n in words[words.len() - 1].split(',').chain(words[..words.len() - 2].iter().filter(|w| w.ends_with(',')).map(|w| w.trim_end_matches(','))) {
                    members.push((ty.to_owned(), n.trim().to_owned()));
                }
            }
            if ok && !members.is_empty() {
                out.insert(name, members);
            }
        }
    }
    out
}

fn struct_varyings(files: &Files, _: &Eval) -> Edits {
    let srcs = sources(files);
    let structs = plain_structs(&srcs);
    if structs.is_empty() {
        return Edits::new();
    }
    let mut edits = Edits::new();
    for (path, text) in &srcs {
        let mut out = text.clone();
        let mut outs: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut ins: Vec<(String, String, Vec<(String, String)>)> = Vec::new();
        let comments = comment_ranges(text);
        // Declarations, last first so offsets hold.
        let mut decls = Vec::new();
        for (sname, members) in &structs {
            for at in find_word(text, sname) {
                if in_comment(&comments, at) {
                    continue;
                }
                let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
                let quals: Vec<&str> = text[ls..at].split_whitespace().collect();
                let dir = if quals.contains(&"out") {
                    "out"
                } else if quals.contains(&"in") {
                    "in"
                } else if quals.contains(&"varying") {
                    if path.ends_with(".fsh") { "in" } else { "out" }
                } else {
                    continue;
                };
                if !quals.iter().all(|q| ["flat", "smooth", "noperspective", "in", "out", "varying"].contains(q)) {
                    continue;
                }
                let rest = &text[at + sname.len()..];
                let var: String = rest.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                let after = rest.trim_start()[var.len()..].trim_start();
                if var.is_empty() || !after.starts_with(';') {
                    continue;
                }
                let semi = at + sname.len() + (rest.len() - after.len());
                decls.push((ls, semi + 1, dir, sname.clone(), var, members.clone(), quals.contains(&"flat")));
            }
        }
        if decls.is_empty() {
            continue;
        }
        decls.sort_by_key(|d| std::cmp::Reverse(d.0));
        for (s, e, dir, sname, var, members, flat) in decls {
            let interp = if flat || members.iter().any(|(t, _)| t.starts_with('i') || t.starts_with('u') || t == "int" || t == "uint") { "flat " } else { "" };
            let fields: String = members.iter().map(|(t, m)| format!("{interp}{dir} {t} hz_{var}_{m}; ")).collect();
            // The marker says the declaration is live (it is often under an `#if`), so the
            // copies in `main` are compiled exactly when it is.
            out.replace_range(s..e, &format!("{fields}{sname} {var};\n#define HZ_HAS_{var}\n"));
            if dir == "out" {
                outs.push((var, members));
            } else {
                ins.push((sname, var, members));
            }
        }
        // The stage's `main` in this file: copies in at its start, out before each return
        // and its closing brace.
        let Some(main) = find_word(&out, "main").into_iter().find(|&m| {
            out[..m].trim_end().ends_with("void") && out[m + 4..].trim_start().starts_with('(')
        }) else {
            continue;
        };
        let Some(open) = out[main..].find('{').map(|i| main + i) else { continue };
        let Some(close) = matching(&out, open) else { continue };
        let copy_out: String = outs
            .iter()
            .map(|(v, ms)| {
                let copies: String = ms.iter().map(|(_, m)| format!("hz_{v}_{m} = {v}.{m}; ")).collect();
                format!("\n#ifdef HZ_HAS_{v}\n{copies}\n#endif\n")
            })
            .collect();
        let copy_in: String = ins
            .iter()
            .map(|(s, v, ms)| {
                let args: Vec<String> = ms.iter().map(|(_, m)| format!("hz_{v}_{m}")).collect();
                format!("\n#ifdef HZ_HAS_{v}\n{v} = {s}({});\n#endif\n", args.join(", "))
            })
            .collect();
        if !copy_out.is_empty() {
            out.insert_str(close, &copy_out);
            for r in find_word(&out[open..close], "return").into_iter().rev() {
                let at = open + r;
                let Some(semi) = out[at..].find(';').map(|i| at + i) else { continue };
                out.insert_str(semi + 1, " }");
                out.insert_str(at, &format!("{{ {copy_out}"));
            }
        }
        if !copy_in.is_empty() {
            out.insert_str(open + 1, &copy_in);
        }
        edits.insert(path.clone(), (out, "struct varyings split into members".into()));
    }
    edits
}

/// Struct types with at least one array member: members as (name, array length).
fn array_structs(srcs: &[(String, String)]) -> BTreeMap<String, Vec<(String, Option<usize>)>> {
    let mut out = BTreeMap::new();
    for (_, text) in srcs {
        for at in find_word(text, "struct") {
            let name: String = text[at + 6..].trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let Some(open) = text[at..].find('{').map(|i| at + i) else { continue };
            if name.is_empty() || text[at + 6..open].trim() != name {
                continue;
            }
            let Some(close) = matching(text, open) else { continue };
            let c = comment_ranges(&text[open + 1..close]);
            let body: String = text[open + 1..close]
                .char_indices()
                .filter(|(i, _)| !in_comment(&c, *i))
                .map(|(_, ch)| ch)
                .collect();
            let mut members = Vec::new();
            let mut ok = true;
            for decl in body.split(';').map(str::trim).filter(|d| !d.is_empty()) {
                if decl.contains('#') || decl.contains(',') {
                    ok = false;
                    break;
                }
                // `float[6] name`, `float name[6]` or `vec3 name`.
                let compact: String = decl.split_whitespace().collect::<Vec<_>>().join(" ");
                let len = |s: &str| s.split('[').nth(1).and_then(|r| r.trim_end_matches(']').trim().parse::<usize>().ok());
                let words: Vec<&str> = compact.split(' ').collect();
                let Some(last) = words.last() else { ok = false; break };
                let (member, n) = if last.contains('[') {
                    (last.split('[').next().unwrap().to_owned(), len(last))
                } else if words.len() >= 2 && words[words.len() - 2].contains('[') {
                    (last.to_string(), len(words[words.len() - 2]))
                } else {
                    (last.to_string(), None)
                };
                if (last.contains('[') || compact.contains('[')) && n.is_none() {
                    ok = false;
                    break;
                }
                members.push((member, n));
            }
            if ok && members.iter().any(|m| m.1.is_some()) {
                out.insert(name, members);
            }
        }
    }
    out
}

/// Brace depth at `at`, outside comments.
fn brace_depth(text: &str, at: usize) -> i32 {
    let comments = comment_ranges(&text[..at]);
    text[..at]
        .char_indices()
        .filter(|(i, _)| !in_comment(&comments, *i))
        .map(|(_, c)| match c {
            '{' => 1,
            '}' => -1,
            _ => 0,
        })
        .sum()
}

fn struct_array_ctor(files: &Files, _: &Eval) -> Edits {
    let srcs = sources(files);
    let structs = array_structs(&srcs);
    if structs.is_empty() {
        return Edits::new();
    }
    let mut edits = Edits::new();
    for (path, text) in &srcs {
        let mut hits: Vec<(usize, usize, String)> = Vec::new();
        for (sname, members) in &structs {
            for at in find_word(text, sname) {
                // `[const] S name = S(` ... `);` inside a function.
                let rest = &text[at + sname.len()..];
                let var: String = rest.trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if var.is_empty() {
                    continue;
                }
                let after_var = rest.trim_start()[var.len()..].trim_start();
                let Some(after_eq) = after_var.strip_prefix('=') else { continue };
                let after_eq = after_eq.trim_start();
                if !after_eq.starts_with(sname.as_str()) {
                    continue;
                }
                let ctor = text.len() - after_eq.len();
                let open = ctor + sname.len() + (text[ctor + sname.len()..].len() - text[ctor + sname.len()..].trim_start().len());
                if text.as_bytes().get(open) != Some(&b'(') || brace_depth(text, at) < 1 {
                    continue;
                }
                let Some(close) = matching(text, open) else { continue };
                if !text[close + 1..].trim_start().starts_with(';') {
                    continue;
                }
                let semi = close + 1 + (text[close + 1..].len() - text[close + 1..].trim_start().len());
                let args = split_args(text, open, close);
                if args.len() != members.len() {
                    continue;
                }
                let mut stmts = format!("{sname} {var};");
                let mut ok = true;
                for ((m, n), arg) in members.iter().zip(&args) {
                    let c = comment_ranges(arg);
                    let arg: String = arg.char_indices().filter(|(i, _)| !in_comment(&c, *i)).map(|(_, ch)| ch).collect();
                    let arg = arg.split_whitespace().collect::<Vec<_>>().join(" ");
                    match n {
                        None => stmts += &format!(" {var}.{m} = {arg};"),
                        Some(_) => {
                            // `T[N](e0, e1, ...)`: one assignment per element.
                            let Some(p) = arg.find('(') else { ok = false; break };
                            let Some(pc) = matching(&arg, p) else { ok = false; break };
                            if !arg[..p].contains('[') || pc + 1 != arg.len() {
                                ok = false;
                                break;
                            }
                            for (j, e) in split_args(&arg, p, pc).iter().enumerate() {
                                stmts += &format!(" {var}.{m}[{j}] = {e};");
                            }
                        }
                    }
                }
                if !ok {
                    continue;
                }
                // The declaration starts at `const` when there is one.
                let start = {
                    let before = text[..at].trim_end();
                    if before.ends_with("const") && before.len() >= 5 && !before[..before.len() - 5].ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                        before.len() - 5
                    } else {
                        at
                    }
                };
                let lines = text[start..=semi].matches('\n').count();
                hits.push((start, semi + 1, format!("{stmts}{}", "\n".repeat(lines))));
            }
        }
        if hits.is_empty() {
            continue;
        }
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        let mut out = text.clone();
        let n = hits.len();
        for (s, e, with) in hits {
            out.replace_range(s..e, &with);
        }
        edits.insert(path.clone(), (out, format!("{n} constructor(s)")));
    }
    edits
}

/// Messages Horizon gives for image and storage-buffer code.
fn image_problem(message: &str) -> bool {
    ["images are not supported", "image types require", "uniform/buffer blocks require", "Bindings for", "colorimg", "imageStore", "imageLoad", "image2D", "image3D"]
        .iter()
        .any(|m| message.contains(m))
}

/// Macros tested positively (`#ifdef X`, `#if defined X`, `#if X`) by the conditionals around
/// line `line` (from 1) of `text`.
pub(crate) fn guards_at(text: &str, line: usize) -> Vec<String> {
    let mut stack: Vec<Vec<String>> = Vec::new();
    for l in text.lines().take(line.saturating_sub(1)) {
        let t = l.trim_start();
        let Some(d) = t.strip_prefix('#') else { continue };
        let d = d.trim_start();
        let words = |s: &str| -> Vec<String> {
            let s = s.split("//").next().unwrap_or("");
            let mut out = Vec::new();
            // `!defined X` and `!X` are negative tests: X is not a switch that turns code on.
            let mut negated = false;
            for tok in s.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '!')) {
                let name = tok.trim_start_matches('!');
                if tok.starts_with('!') {
                    negated = true;
                }
                if name.is_empty() || name == "defined" {
                    continue;
                }
                if !negated && name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_') {
                    out.push(name.to_owned());
                }
                negated = false;
            }
            out
        };
        if let Some(r) = d.strip_prefix("ifdef") {
            stack.push(words(r));
        } else if d.starts_with("ifndef") {
            stack.push(Vec::new());
        } else if let Some(r) = d.strip_prefix("if") {
            stack.push(words(r));
        } else if d.starts_with("elif") || d.starts_with("else") {
            if let Some(top) = stack.last_mut() {
                top.clear();
            }
        } else if d.starts_with("endif") {
            stack.pop();
        }
    }
    stack.into_iter().flatten().collect()
}

fn images_off(files: &Files, best: &Eval) -> Edits {
    let srcs = sources(files);
    let mut guards = BTreeSet::new();
    for skip in best.unique_skips() {
        for p in skip.problems.iter().filter(|p| image_problem(&p.message)) {
            if let Some((_, text)) = srcs.iter().find(|(f, _)| *f == p.file) {
                guards.extend(guards_at(text, p.line as usize));
            }
        }
    }
    let mut edits = Edits::new();
    for name in guards {
        // Its `#define`s outside stage files (a stage file's own defines say which program
        // it is), as a plain switch: `#define X` or `#define X 1`.
        for (path, text) in &srcs {
            if is_stage(path) {
                continue;
            }
            let mut out = edits.get(path).map(|e| e.0.clone()).unwrap_or_else(|| text.clone());
            let mut n = 0;
            let mut lines: Vec<String> = out.split('\n').map(str::to_owned).collect();
            for l in lines.iter_mut() {
                let t = l.trim_start();
                let Some(d) = t.strip_prefix('#') else { continue };
                let Some(rest) = d.trim_start().strip_prefix("define") else { continue };
                let mut parts = rest.split_whitespace();
                if parts.next() != Some(name.as_str()) {
                    continue;
                }
                let value = parts.next();
                if !matches!(value, None | Some("1")) && !value.is_some_and(|v| v.starts_with("//")) {
                    continue;
                }
                *l = format!("// horizon-shaderport: {name} turned off (Horizon has no images yet): {}", l.trim());
                n += 1;
            }
            if n > 0 {
                out = lines.join("\n");
                edits.insert(path.clone(), (out, format!("`{name}` turned off")));
            }
        }
    }
    edits
}

fn missing_return(files: &Files, best: &Eval) -> Edits {
    // `Function [12] 'name' is invalid: The `return` expression None does not match...`
    let mut wanted: BTreeSet<(String, String)> = BTreeSet::new();
    for skip in best.unique_skips() {
        for p in &skip.problems {
            if !p.message.contains("The `return` expression None does not match") {
                continue;
            }
            let Some(name) = p.message.split('\'').nth(1) else { continue };
            wanted.insert((p.file.clone(), name.to_owned()));
        }
    }
    let mut edits = Edits::new();
    for (file, name) in wanted {
        let Some(mut text) = edits.get(&file).map(|e| e.0.clone()).or_else(|| files.text(&file)) else {
            continue;
        };
        // The definition: `TYPE name(...) {`.
        let Some((ty, close)) = find_word(&text, &name).into_iter().find_map(|at| {
            let open = at + name.len() + (text[at + name.len()..].len() - text[at + name.len()..].trim_start().len());
            if text.as_bytes().get(open) != Some(&b'(') {
                return None;
            }
            let ty = text[..at].trim_end().rsplit(|c: char| c.is_whitespace()).next()?.to_owned();
            let pclose = matching(&text, open)?;
            let body = pclose + 1 + (text[pclose + 1..].len() - text[pclose + 1..].trim_start().len());
            if text.as_bytes().get(body) != Some(&b'{') {
                return None;
            }
            Some((ty, matching(&text, body)?))
        }) else {
            continue;
        };
        let builtin = ty.starts_with("vec") || ty.starts_with("ivec") || ty.starts_with("uvec")
            || ty.starts_with("mat") || ["float", "int", "uint"].contains(&ty.as_str());
        if !builtin {
            continue;
        }
        text.insert_str(close, &format!(" return {ty}(0); "));
        edits.insert(file, (text, format!("`{name}` returns {ty}(0) at its end")));
    }
    edits
}

fn int_vector_modulo(files: &Files, _: &Eval) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let mut hits = Vec::new();
        for ctor in ["ivec2", "ivec3", "ivec4", "uvec2", "uvec3", "uvec4"] {
            for at in find_word(&text, ctor) {
                let open = at + ctor.len();
                if text.as_bytes().get(open) != Some(&b'(') {
                    continue;
                }
                let Some(close) = matching(&text, open) else { continue };
                let rest = &text[close + 1..];
                let ws = rest.len() - rest.trim_start().len();
                let pct = close + 1 + ws;
                if text.as_bytes().get(pct) != Some(&b'%') || text.as_bytes().get(pct + 1) == Some(&b'=') {
                    continue;
                }
                let r0 = pct + 1 + (text[pct + 1..].len() - text[pct + 1..].trim_start().len());
                let Some(r1) = operand_end(&text, r0) else { continue };
                let operand = &text[r0..r1];
                if operand.starts_with(ctor) {
                    continue;
                }
                hits.push((r0, r1, ctor));
            }
            // Names declared with this type in the file: `ivec2 texel = ...; texel % n`.
            let mut names = BTreeSet::new();
            for at in find_word(&text, ctor) {
                let rest = text[at + ctor.len()..].trim_start();
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() && !rest.starts_with('(') {
                    names.insert(name);
                }
            }
            for name in names {
                for at in find_word(&text, &name) {
                    let end = at + name.len();
                    let rest = &text[end..];
                    let ws = rest.len() - rest.trim_start().len();
                    let pct = end + ws;
                    if text.as_bytes().get(pct) != Some(&b'%') {
                        continue;
                    }
                    let op_end = if text.as_bytes().get(pct + 1) == Some(&b'=') { pct + 2 } else { pct + 1 };
                    let r0 = op_end + (text[op_end..].len() - text[op_end..].trim_start().len());
                    let Some(r1) = operand_end(&text, r0) else { continue };
                    if text[r0..r1].starts_with(ctor) {
                        continue;
                    }
                    hits.push((r0, r1, ctor));
                }
            }
        }
        if hits.is_empty() {
            continue;
        }
        hits.sort();
        hits.dedup_by_key(|h| h.0);
        let mut out = text.clone();
        let n = hits.len();
        for (s, e, ctor) in hits.into_iter().rev() {
            out.replace_range(s..e, &format!("{ctor}({})", &text[s..e]));
        }
        edits.insert(path, (out, format!("{n} modulo(s)")));
    }
    edits
}

/// The end of a primary expression starting at `s`: a number, or a name with calls, indexing
/// and member accesses, or a parenthesised expression.
fn operand_end(text: &str, s: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut i = s;
    if b.get(i) == Some(&b'(') {
        return matching(text, i).map(|c| c + 1);
    }
    let start = i;
    while i < b.len() && (is_ident_byte(b[i]) || b[i] == b'.') {
        i += 1;
    }
    if i == start {
        return None;
    }
    loop {
        match b.get(i) {
            Some(b'(') | Some(b'[') => i = matching(text, i)? + 1,
            Some(b'.') => {
                i += 1;
                while i < b.len() && is_ident_byte(b[i]) {
                    i += 1;
                }
            }
            _ => break,
        }
    }
    Some(i)
}

pub fn report(name: &str, before: &Eval, after: &Eval, log: &[Change]) -> String {
    let mut s = format!("# {name} for Horizon\n\nGenerated by horizon-shaderport. Converted packs are for your own use: shader pack licences often forbid sharing modified copies.\n\n");
    s += &format!("- Before: {}\n- After: {}\n\n", before.summary(), after.summary());
    if !log.is_empty() {
        s += "## Rewrites\n\n";
        for c in log {
            let why = RULES.iter().find(|r| r.name == c.rule).map_or(
                if c.rule == crate::shaderport::normals::NAME { crate::shaderport::normals::WHY } else if c.rule == crate::shaderport::uniforms::NAME { crate::shaderport::uniforms::WHY } else { "" },
                |r| r.why,
            );
            s += &format!("- **{}**: {}. {}", c.rule, why, c.outcome);
            if !c.files.is_empty() {
                let shown: Vec<_> = c.files.iter().take(8).map(|f| format!("`{f}`")).collect();
                s += &format!(" in {}{}", shown.join(", "), if c.files.len() > 8 { format!(" and {} more", c.files.len() - 8) } else { String::new() });
            }
            s += ".\n";
        }
        s += "\n";
    }
    let left = after.unique_skips();
    if !left.is_empty() {
        s += "## Still skipped, and why\n\n";
        for k in &left {
            let first = k.problems.first().map_or(
                if k.geometry { "uses a geometry stage, which Horizon does not run".to_owned() } else { "(no message)".into() },
                |p| format!("{}:{}: {}", p.file, p.line, p.message),
            );
            s += &format!("- `{}`: {}\n", k.program, first);
        }
        s += "\n";
    }
    s += "## What Horizon loads now\n\n```\n";
    s += &after.describe(name);
    s += "```\n";
    s
}
