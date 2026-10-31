# Horizon Android Plus: dynamic launcher + shader compatibility tools

This ZIP contains source patches and a standalone Python tool for the `android` branch of `CihuyAkz/horizon-android-plus`.

## Changes

- Adds slow ambient particles, pulsing logo glow, animated active-tab underline, and a pulsing home card edge.
- Updates the launcher header to `Horizon Android Plus` on wider layouts and `Horizon+` on narrow layouts to avoid colliding with the account button.
- Keeps the Android window title and Android Files provider root named `Horizon Android Plus`.
- Changes the shader import action to `Check & Convert` and explains the compatibility scan in the shader page.
- Automatically checks classic Iris/OptiFine ZIP shader imports after safe extraction.
- Applies the verified `cloudHeight` conversion only when the exact source pattern is present in `shaders/lib/common.glsl`.
- Leaves already-converted shader source unchanged. It also leaves unknown shader versions unchanged and tells the user no verified rule matched.
- Leaves native `.hshader` validation on Horizon's existing manifest and API checker.

## Apply the app patches

Extract this ZIP into the root of your repository, then run these commands from the repository root:

```bash
git apply patches/0001-horizon-shader-check-and-convert.patch
git apply patches/0002-dynamic-launcher-and-smart-shader-ui.patch
git apply patches/0003-horizon-android-plus-window-and-files-name.patch
```

The patches target the `android` branch layout that was inspected for this change. They modify only:

- `crates/horizon-client/src/content_files.rs`
- `crates/horizon-client/src/android/documents.rs`
- `crates/horizon-client/src/android/launcher_pages.rs`
- `crates/horizon-client/src/android/launcher.rs`
- `crates/horizon-client/src/android/document_store.rs`

## Standalone converter

Requires Python 3.10 or newer and uses only the Python standard library.

Check a shader ZIP:

```bash
python tools/horizon_shader_converter.py check ComplementaryReimagined_r5.9.3.zip
```

Convert it:

```bash
python tools/horizon_shader_converter.py convert ComplementaryReimagined_r5.9.3.zip ComplementaryHorizon.zip
```

The tool writes a top-level `ComplementaryDev/` folder when the input ZIP has no enclosing pack folder, matching the structure of the supplied working Horizon example. If the input already has one enclosing pack folder, the tool preserves that folder name. The app's extraction path removes that wrapper when installing the pack.

## Verified conversion scope

The supplied original and working Horizon ZIPs were compared entry by entry. All 436 file paths and contents matched after normalization except one exact line in `shaders/lib/common.glsl`. The converter implements only that proven rule:

```glsl
float cloudHeightM = isnan(cloudHeight) ? 192.0 : cloudHeight;
```

becomes:

```glsl
float cloudHeightM = (isnan(cloudHeight) || cloudHeight < 1.0) ? 192.0 : cloudHeight;
```

The generated replacement also preserves the comment from the working example. No other shader source is rewritten. This is a verified first rule for the supplied Complementary shader version, not a claim that every Iris/OptiFine shader is convertible with one universal transformation. Unknown versions are not rewritten.

## Tests

Run the converter tests from this folder:

```bash
python -m unittest discover -s tests -v
```

After applying the Rust patches, run your Android GitHub Actions build. The Python tests validate the converter and ZIP behavior; they do not replace a full Android/Rust compile or in-game shader test.
