//! Pass `fragdepth-read`. A file that reads `gl_FragDepth` but never writes it (Noble's water:
//! `calculateShadowMapping(scenePosition, tbn[2], gl_FragDepth)`) gets the fragment's own depth in
//! GL, since nothing wrote another one. In Horizon the mention makes the program write a depth of
//! its own, left unset: water under the sand was drawn over it wherever the shadow pass is on.
//! Such reads become `gl_FragCoord.z`.

use crate::shaderport::files::Files;
use crate::shaderport::fix::{Change, Edits, sources};
use crate::shaderport::glsl::{comment_ranges, find_word, in_comment};

fn is_write(text: &str, at: usize) -> bool {
    let rest = text[at + "gl_FragDepth".len()..].trim_start();
    rest.starts_with('=') && !rest.starts_with("==")
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let mut edits = Edits::new();
    for (path, text) in sources(files) {
        let comments = comment_ranges(&text);
        let hits: Vec<usize> = find_word(&text, "gl_FragDepth").into_iter().filter(|&a| !in_comment(&comments, a)).collect();
        if hits.is_empty() || hits.iter().any(|&a| is_write(&text, a)) {
            continue;
        }
        let mut out = text.clone();
        for &a in hits.iter().rev() {
            out.replace_range(a..a + "gl_FragDepth".len(), "gl_FragCoord.z");
        }
        edits.insert(path, (out, format!("{} read(s)", hits.len())));
    }
    if edits.is_empty() {
        return;
    }
    let Ok(best) = crate::shaderport::load::evaluate(files) else { return };
    let kept = crate::shaderport::pbr::keep(files, &best, edits, 0);
    println!("  fragdepth-read: {} file(s)", kept.len());
    log.push(Change {
        rule: "fragdepth-read",
        files: kept.into_iter().collect(),
        outcome: "`gl_FragDepth` read but never written becomes `gl_FragCoord.z` (in Horizon the mention makes the program write an unset depth)".into(),
    });
}
