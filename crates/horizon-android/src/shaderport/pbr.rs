//! Pass `pbr-defaults`. Horizon binds a 1×1 white stand-in for the `normals` and `specular`
//! atlases (it loads no PBR resource packs), so a pack reads every block as a perfect mirror with a
//! bent normal (Noble: black patches and a mirror sheen on stone). Iris, with no PBR pack loaded,
//! gives a flat normal (0.5, 0.5, 1, 1) and no specular (0, 0, 0, 0). The two samplers become
//! constants with those values, read through `texture()` overloads, so the pack's code is unchanged.

use std::collections::BTreeSet;

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Change, Edits, insert_decl, sources, stages_using};
use crate::shaderport::glsl::{comment_ranges, find_word, in_comment};
use crate::shaderport::load::{Eval, evaluate};

const MARK: &str = "// horizon-shaderport pbr-defaults";

const POLYFILL: &str = "\
// horizon-shaderport pbr-defaults: no PBR atlases in Horizon; the values Iris gives without them
#ifndef HZ_CTEX
#define HZ_CTEX
struct hz_ctex { vec4 v; };
#ifndef texture
vec4 texture(hz_ctex t, vec2 p) { return t.v; }
vec4 texture(hz_ctex t, vec2 p, float bias) { return t.v; }
#endif
vec4 textureLod(hz_ctex t, vec2 p, float lod) { return t.v; }
vec4 textureGrad(hz_ctex t, vec2 p, vec2 dx, vec2 dy) { return t.v; }
vec4 texelFetch(hz_ctex t, ivec2 p, int lod) { return t.v; }
ivec2 textureSize(hz_ctex t, int lod) { return ivec2(1); }
#endif
";

fn value(name: &str) -> &'static str {
    if name == "normals" { "vec4(0.5, 0.5, 1.0, 1.0)" } else { "vec4(0.0)" }
}

fn edits(files: &Files) -> Edits {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let comments = comment_ranges(&text);
        let mut hits = Vec::new();
        for name in ["normals", "specular"] {
            for at in find_word(&text, name) {
                if in_comment(&comments, at) {
                    continue;
                }
                let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                let before: Vec<&str> = text[line_start..at].split_whitespace().collect();
                if before != ["uniform", "sampler2D"] {
                    continue;
                }
                let Some(semi) = text[at..].find(';').map(|i| at + i) else { continue };
                if !text[at + name.len()..semi].trim().is_empty() {
                    continue;
                }
                let indent = line_start + (text[line_start..].len() - text[line_start..].trim_start().len());
                hits.push((indent, semi + 1, format!("const hz_ctex {name} = hz_ctex({});", value(name))));
            }
        }
        if hits.is_empty() {
            continue;
        }
        hits.sort_by_key(|h| h.0);
        let mut out = text.clone();
        for (s, e, r) in hits.iter().rev() {
            out.replace_range(*s..*e, r);
        }
        edits.insert(path, (out, format!("{} sampler(s)", hits.len())));
    }
    if edits.is_empty() {
        return edits;
    }
    let mut after = Files { name: files.name.clone(), entries: files.entries.clone(), layout_notes: Vec::new() };
    for (k, (t, _)) in &edits {
        after.set_text(k, t.clone());
    }
    for stage in stages_using(&after, "hz_ctex") {
        let text = after.text(&stage).unwrap_or_default();
        if text.contains(MARK) {
            continue;
        }
        let entry = edits.entry(stage).or_insert_with(|| (text.clone(), String::new()));
        entry.0 = insert_decl(&entry.0, POLYFILL);
    }
    edits
}

fn lost(before: &Eval, after: &Eval) -> bool {
    before
        .dims
        .iter()
        .zip(&after.dims)
        .any(|(b, a)| b.loaded.iter().any(|p| !a.loaded.contains(p)))
        || after.panicked()
}

/// Applies `edits`, keeping what does not make a loading program fail (halves on failure).
pub(crate) fn keep(files: &mut Files, best: &Eval, edits: Edits, depth: u32) -> BTreeSet<String> {
    let saved: Vec<(String, Option<Vec<u8>>)> = edits.keys().map(|k| (k.clone(), files.entries.get(&format!("shaders/{k}")).cloned())).collect();
    for (k, (t, _)) in &edits {
        files.set_text(k, t.clone());
    }
    if evaluate(files).is_ok_and(|e| !lost(best, &e)) {
        return edits.into_keys().collect();
    }
    for (k, d) in saved {
        files.restore(&k, d);
    }
    if edits.len() < 2 || depth >= 5 {
        return BTreeSet::new();
    }
    let mut first = edits;
    let mid = first.keys().nth(first.len() / 2).unwrap().clone();
    let second = first.split_off(&mid);
    let mut kept = keep(files, best, first, depth + 1);
    kept.extend(keep(files, best, second, depth + 1));
    kept
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let edits = edits(files);
    if edits.is_empty() {
        return;
    }
    let Ok(best) = evaluate(files) else { return };
    let total = edits.len();
    let kept = keep(files, &best, edits, 0);
    println!("  pbr-defaults: {} of {total} file(s) kept", kept.len());
    log.push(Change {
        rule: "pbr-defaults",
        files: kept.into_iter().collect(),
        outcome: "`normals`/`specular` read Iris's no-PBR values (flat normal, no specular) instead of Horizon's white stand-in".into(),
    });
}
