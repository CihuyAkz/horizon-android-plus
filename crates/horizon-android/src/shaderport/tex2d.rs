//! Pass `custom-tex2d`. The game loads no custom texture files (nightly 7aa0e28): a pack's
//! `texture.<stage>.colortexN = image.png` leaves `colortexN` the pack's own buffer in that stage.
//! SuperDuperVanilla binds its cloud map `clouds.png` to `colortex0` in deferred and composite
//! and reads the cloud shapes from it; in Horizon it read the scene colour, so every bright pixel
//! was a cloud and the sky was one pink haze.
//!
//! A small image is built into the shader exactly: each channel's values become a palette (1, 2
//! or 8 bits), packed into a `uint` array in a generated include, read with `texelFetch`,
//! `texture` (nearest or bilinear as its `.mcmeta` says, repeating unless it clamps), `textureLod`
//! and `textureSize`. The sampler becomes a `bvec2`/`bvec4` constant those overloads take.
//!
//! Only programs that run before anything in that stage writes the buffer read the image (a
//! later one, like SDV's bloom passes reusing `colortex0`, reads what was written).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use horizon_shaders::{Group, Macros, Properties, buffer_index, dimension_folder, load_post};

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Change, Edits, insert_decl, sources, stages_using};
use crate::shaderport::glsl::{comment_ranges, find_word, in_comment};
use crate::shaderport::noise2d::{Image, read_png};

/// Largest image data built in, in `uint`s (64 KiB).
const MAX_WORDS: usize = 16384;
const TYPES: [&str; 2] = ["bvec2", "bvec4"];

struct Entry {
    stage: String,
    name: String,
    file: String,
}

fn entries(properties: &Properties) -> Vec<Entry> {
    let mut out = Vec::new();
    for e in &properties.entries {
        let parts: Vec<&str> = e.key.trim().split('.').collect();
        let ["texture", stage, name] = parts.as_slice() else { continue };
        let file = e.value.trim();
        if *name == "noisetex" || file.contains(':') || file.split_whitespace().count() != 1 || !file.ends_with(".png") {
            continue;
        }
        if !matches!(*stage, "deferred" | "composite" | "prepare") || buffer_index(name).is_none() {
            continue;
        }
        out.push(Entry { stage: stage.to_string(), name: name.to_string(), file: file.to_owned() });
    }
    out
}

/// `blur` and `clamp` from the image's `.mcmeta` (both false without one).
fn mcmeta(files: &Files, file: &str) -> (bool, bool) {
    let Some(t) = files.text(&format!("{}.mcmeta", file.trim_start_matches('/'))) else { return (false, false) };
    let t: String = t.chars().filter(|c| !c.is_whitespace()).collect();
    (t.contains("\"blur\":true"), t.contains("\"clamp\":true"))
}

fn num(x: f32) -> String {
    format!("{x:.6}")
}

/// The include: data, texel reader and overloads for `ty`. `None` when too big.
fn include(img: &Image, name: &str, ty: &str, blur: bool, clamp: bool) -> Option<(String, String)> {
    let [w, h] = img.size;
    // Channels: palette of 8-bit values, bits, and a channel it repeats (same data).
    let byte = |t: &[f32; 4], k: usize| (t[k] * 255.0).round() as u8;
    let mut fields: Vec<(Vec<u8>, u32, Option<usize>)> = Vec::new();
    for k in 0..4 {
        let same = (0..k).find(|&j| img.texels.iter().all(|t| byte(t, j) == byte(t, k)));
        let mut pal: Vec<u8> = img.texels.iter().map(|t| byte(t, k)).collect::<BTreeSet<_>>().into_iter().collect();
        let bits = match pal.len() {
            1 => 0,
            2 => 1,
            3..=4 => 2,
            _ => {
                pal = (0..=255).collect();
                8
            }
        };
        fields.push((pal, if same.is_some() { 0 } else { bits }, same));
    }
    let used: u32 = fields.iter().map(|f| f.1).sum();
    let per_texel = [1u32, 2, 4, 8, 16, 32].into_iter().find(|&b| b >= used.max(1))?;
    let per_word = (32 / per_texel) as usize;
    let words = (w * h).div_ceil(per_word);
    if words > MAX_WORDS {
        return None;
    }
    let mut data = vec![0u32; words];
    for y in 0..h {
        for x in 0..w {
            // GL's row 0 is the image's bottom row.
            let t = &img.texels[(h - 1 - y) * w + x];
            let mut v = 0u32;
            let mut off = 0;
            for (k, (pal, bits, same)) in fields.iter().enumerate() {
                if *bits == 0 || same.is_some() {
                    continue;
                }
                let idx = pal.iter().position(|&p| p == byte(t, k)).unwrap_or(0) as u32;
                v |= idx << off;
                off += bits;
            }
            let i = y * w + x;
            data[i / per_word] |= v << ((i % per_word) as u32 * per_texel);
        }
    }
    let p = format!("hz_c2_{name}");
    let mut s = format!("// horizon-shaderport custom-tex2d: {} ({w}x{h}) built in; the game loads no custom textures\n", img.file);
    let _ = write!(s, "#ifndef HZ_C2_DONE_{name}\n#define HZ_C2_DONE_{name}\nuint {p}_d[{words}] = uint[{words}](");
    for (i, d) in data.iter().enumerate() {
        if i % 12 == 0 {
            s.push('\n');
        }
        let _ = write!(s, "0x{d:08x}u{}", if i + 1 < words { "," } else { "" });
    }
    s.push_str(");\n");
    let wrap = if clamp {
        format!("clamp(c, ivec2(0), ivec2({}, {}))", w - 1, h - 1)
    } else {
        format!("ivec2(c.x - {w} * int(floor(float(c.x) / {w}.0)), c.y - {h} * int(floor(float(c.y) / {h}.0)))")
    };
    let _ = writeln!(
        s,
        "vec4 {p}_t(ivec2 c) {{\n    c = {wrap};\n    int i = c.y * {w} + c.x;\n    uint v = ({p}_d[i / {per_word}] >> uint((i % {per_word}) * {per_texel})) & {}u;",
        if per_texel == 32 { u32::MAX } else { (1u32 << per_texel) - 1 }
    );
    let mut chans = Vec::new();
    let mut off = 0;
    for (pal, bits, same) in &fields {
        if let Some(j) = same {
            chans.push(format!("c{j}"));
            continue;
        }
        let f = |b: &u8| num(*b as f32 / 255.0);
        let e = match bits {
            0 => f(&pal[0]),
            8 => format!("float((v >> {off}u) & 255u) / 255.0"),
            _ => {
                let idx = format!("((v >> {off}u) & {}u)", (1u32 << bits) - 1);
                let mut e = f(pal.last().unwrap());
                for (i, b) in pal.iter().enumerate().rev().skip(1) {
                    e = format!("({idx} == {i}u ? {} : {e})", f(b));
                }
                e
            }
        };
        off += bits;
        chans.push(e);
    }
    for (k, c) in chans.iter().enumerate() {
        let c = if c.starts_with('c') && c.len() == 2 { c.clone() } else { c.clone() };
        let _ = writeln!(s, "    float c{k} = {c};");
    }
    s.push_str("    return vec4(c0, c1, c2, c3);\n}\n");
    let sample = if blur {
        format!(
            "vec2 q = p * vec2({w}.0, {h}.0) - 0.5; ivec2 b = ivec2(floor(q)); vec2 f = q - floor(q);\n    \
             return mix(mix({p}_t(b), {p}_t(b + ivec2(1, 0)), f.x), mix({p}_t(b + ivec2(0, 1)), {p}_t(b + ivec2(1, 1)), f.x), f.y);"
        )
    } else {
        format!("return {p}_t(ivec2(floor(p * vec2({w}.0, {h}.0))));")
    };
    let _ = write!(
        s,
        "vec4 {p}_s(vec2 p) {{\n    {sample}\n}}\n\
         #ifndef texture\n\
         vec4 texture({ty} t, vec2 p) {{ return {p}_s(p); }}\n\
         vec4 texture({ty} t, vec2 p, float bias) {{ return {p}_s(p); }}\n\
         #endif\n\
         vec4 textureLod({ty} t, vec2 p, float lod) {{ return {p}_s(p); }}\n\
         vec4 textureGrad({ty} t, vec2 p, vec2 dx, vec2 dy) {{ return {p}_s(p); }}\n\
         vec4 texelFetch({ty} t, ivec2 c, int lod) {{ return {p}_t(c); }}\n\
         ivec2 textureSize({ty} t, int lod) {{ return max(ivec2({w}, {h}) >> lod, ivec2(1)); }}\n\
         #endif\n"
    );
    let how = format!("{} {w}x{h} as {name} ({} bit(s) per texel, {})", img.file, per_texel, if blur { "bilinear" } else { "nearest" });
    Some((s, how))
}

/// `uniform sampler2D <name>;` statements in `text` (start, end after `;`).
fn declarations(text: &str, name: &str) -> Vec<(usize, usize)> {
    let comments = comment_ranges(text);
    let mut out = Vec::new();
    for at in find_word(text, name) {
        if in_comment(&comments, at) {
            continue;
        }
        let ls = text[..at].rfind('\n').map_or(0, |i| i + 1);
        let before: Vec<&str> = text[ls..at].split_whitespace().collect();
        if before != ["uniform", "sampler2D"] {
            continue;
        }
        let Some(semi) = text[at..].find(';').map(|i| at + i) else { continue };
        if !text[at + name.len()..semi].trim().is_empty() {
            continue;
        }
        let start = ls + (text[ls..].len() - text[ls..].trim_start().len());
        out.push((start, semi + 1));
    }
    out
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let Ok(pack) = files.pack() else { return };
    let ptext = files.text("shaders.properties").unwrap_or_default();
    let properties = Properties::parse(&ptext, &crate::shaderport::bufsize::option_macros(files));
    let mut list = entries(&properties);
    if list.is_empty() {
        return;
    }
    // One stand-in per sampler name (the overload type is per name).
    let mut names: Vec<String> = Vec::new();
    for e in &list {
        if !names.contains(&e.name) {
            names.push(e.name.clone());
        }
    }
    names.truncate(TYPES.len());
    list.retain(|e| names.contains(&e.name));
    let inputs = crate::shaderport::load::inputs();
    let plain = Macros::standard();
    // Stage files that read each name's image: before anything in the stage writes the buffer.
    let mut targets: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new(); // name -> (stage file, image)
    for id in [0, -1, 1] {
        let folder = dimension_folder(&pack, id);
        let Ok(post) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_post(&pack, &folder, &properties, &plain, &inputs)
        })) else {
            continue;
        };
        for e in &list {
            let Some(buf) = buffer_index(&e.name) else { continue };
            let group = match e.stage.as_str() {
                "deferred" => Group::Deferred,
                "composite" => Group::Composite,
                _ => Group::Prepare,
            };
            let mut written = false;
            for p in post.programs.iter().filter(|p| p.group == group || (e.stage == "composite" && p.group == Group::Final)) {
                if !written {
                    let stem = [format!("{folder}/{}", p.name), p.name.clone()]
                        .into_iter()
                        .find(|s| !s.starts_with('/') && files.text(&format!("{s}.fsh")).is_some());
                    if let Some(stem) = stem {
                        let t = targets.entry(e.name.clone()).or_default();
                        let f = format!("{stem}.fsh");
                        if !t.iter().any(|(x, _)| *x == f) {
                            t.push((f, e.file.clone()));
                        }
                    }
                }
                if p.buffers.contains(&(buf as u32)) {
                    written = true;
                }
            }
        }
    }
    let mut edits = Edits::new();
    let mut used = Vec::new();
    for (k, name) in names.iter().enumerate() {
        let Some(t) = targets.get(name) else { continue };
        let reading = stages_using(files, name);
        let ty = TYPES[k];
        let mut includes: BTreeMap<String, String> = BTreeMap::new(); // image -> include path
        for (stage_file, file) in t {
            if !reading.contains(stage_file) {
                continue;
            }
            let inc = match includes.get(file) {
                Some(i) => i.clone(),
                None => {
                    let Some(img) = read_png(files, file) else { continue };
                    let (blur, clamp) = mcmeta(files, file);
                    let Some((src, how)) = include(&img, name, ty, blur, clamp) else { continue };
                    let path = format!("hz_c2_{name}_{}.glsl", includes.len());
                    edits.insert(path.clone(), (src, "custom texture data".into()));
                    used.push(how);
                    includes.insert(file.clone(), path.clone());
                    path
                }
            };
            let base = edits.get(stage_file).map(|e| e.0.clone()).or_else(|| files.text(stage_file)).unwrap_or_default();
            let decl = format!("#define HZ_C2_{name}\n#include \"/{inc}\"\n");
            edits.insert(stage_file.clone(), (insert_decl(&base, &decl), format!("reads {file} as {name}")));
        }
        if !edits.keys().any(|k| k.ends_with(".fsh")) {
            continue;
        }
        // The declarations: the constant where the stage defines HZ_C2_<name>.
        for (path, text) in sources(files) {
            let base = edits.get(&path).map(|e| e.0.clone()).unwrap_or(text);
            let decls = declarations(&base, name);
            if decls.is_empty() {
                continue;
            }
            let mut out = base;
            for (s, e) in decls.into_iter().rev() {
                out.replace_range(
                    s..e,
                    &format!("\n#ifdef HZ_C2_{name}\nconst {ty} {name} = {ty}(true);\n#else\nuniform sampler2D {name};\n#endif\n"),
                );
            }
            edits.insert(path, (out, format!("{name}: custom texture stand-in where the stage reads the image")));
        }
    }
    if used.is_empty() {
        return;
    }
    let Ok(best) = crate::shaderport::load::evaluate(files) else { return };
    let total = edits.len();
    if std::env::var_os("HZ_TRACE").is_some() {
        let mut trial = Files { name: files.name.clone(), entries: files.entries.clone(), layout_notes: Vec::new() };
        for (k, (t, _)) in &edits {
            trial.set_text(k, t.clone());
        }
        if let Ok(e) = crate::shaderport::load::evaluate(&trial) {
            for d in &e.dims {
                for s in d.skipped.iter().take(3) {
                    for p in s.problems.iter().take(2) {
                        eprintln!("  [trace] custom-tex2d {}: {}:{}: {}", s.program, p.file, p.line, p.message);
                    }
                }
            }
        }
    }
    // All or nothing: a stage file without its include would not compile.
    let kept = crate::shaderport::pbr::keep(files, &best, edits, 5);
    println!("  custom-tex2d: {} of {total} file(s) kept ({})", kept.len(), used.join("; "));
    if kept.is_empty() {
        return;
    }
    log.push(Change {
        rule: "custom-tex2d",
        files: kept.into_iter().collect(),
        outcome: format!("the pack's own image is built into the shader where a stage reads it: {}", used.join("; ")),
    });
}
