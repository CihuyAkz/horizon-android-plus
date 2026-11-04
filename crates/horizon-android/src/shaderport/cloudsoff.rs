//! Pass `clouds-off`. `clouds=off` in `shaders.properties` tells the game not to draw its clouds.
//! Horizon (nightly 7aa0e28) does not read it: it leaves its clouds out only when the pack has a
//! `gbuffers_clouds` program, which has no fallback. Bliss draws its own volumetric clouds and has
//! none, so the game's blocky clouds show through them, shaded as terrain.
//!
//! Each dimension folder the game loads without a `gbuffers_clouds` gets one that draws nothing.

use horizon_shaders::{Macros, Properties, dimension_folder, load_post};

use crate::shaderport::files::Files;
use crate::shaderport::fix::Change;

const MARK: &str = "// horizon-shaderport clouds-off: the pack sets clouds=off; this program draws nothing";

fn vsh() -> String {
    format!("#version 120\n{MARK}\nvoid main() {{\n    gl_Position = vec4(2.0, 2.0, 2.0, 1.0);\n}}\n")
}

fn fsh() -> String {
    format!("#version 120\n{MARK}\nvoid main() {{\n    discard;\n}}\n")
}

pub fn fix(files: &mut Files, log: &mut Vec<Change>) {
    let ptext = files.text("shaders.properties").unwrap_or_default();
    let properties = Properties::parse(&ptext, &Macros::standard());
    if properties.get("clouds").map(str::trim) != Some("off") {
        return;
    }
    let Ok(pack) = files.pack() else { return };
    let inputs = crate::shaderport::load::inputs();
    let mut added = Vec::new();
    for id in [0, -1, 1] {
        let folder = dimension_folder(&pack, id);
        let Ok(post) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            load_post(&pack, &folder, &properties, &Macros::standard(), &inputs)
        })) else {
            continue;
        };
        if post.clouds.is_some() {
            continue;
        }
        let prefix = if folder.is_empty() { String::new() } else { format!("{folder}/") };
        for (ext, text) in [("vsh", vsh()), ("fsh", fsh())] {
            let path = format!("{prefix}gbuffers_clouds.{ext}");
            if files.text(&path).is_none() && !added.contains(&path) {
                files.set_text(&path, text);
                added.push(path);
            }
        }
    }
    if added.is_empty() {
        return;
    }
    // Kept only if the game now loads a clouds program in every folder it was added to.
    let ok = files.pack().is_ok_and(|pack| {
        [0, -1, 1].iter().all(|&id| {
            let folder = dimension_folder(&pack, id);
            let prefix = if folder.is_empty() { String::new() } else { format!("{folder}/") };
            if !added.contains(&format!("{prefix}gbuffers_clouds.fsh")) {
                return true;
            }
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                load_post(&pack, &folder, &properties, &Macros::standard(), &inputs)
            }))
            .is_ok_and(|p| p.clouds.is_some())
        })
    });
    if !ok {
        for p in &added {
            files.remove(p);
        }
        println!("  clouds-off: not kept (the added program did not load)");
        return;
    }
    println!("  clouds-off: {} empty clouds program file(s) added", added.len());
    log.push(Change {
        rule: "clouds-off",
        files: added,
        outcome: "the pack sets `clouds=off`, which the game does not read: it hides its own clouds only when the pack has `gbuffers_clouds`, so an empty one is added (the game's clouds no longer show through the pack's sky)".to_owned(),
    });
}
