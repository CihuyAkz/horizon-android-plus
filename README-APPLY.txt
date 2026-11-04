HORIZON ANDROID — FINAL FIX OVERLAY

Prepared for the CihuyAkz/horizon-android-plus Android source tree.

WHAT'S INCLUDED
- Shader ZIP import validation and extraction, including support for a root shaders/ folder or one wrapper directory.
- Installation/removal of extracted shader-pack folders.
- Updated launcher UI, creator credits and download progress visuals.
- Larger inner joystick knob (outer joystick frame unchanged).
- Android APK workflow using platform-tools instead of the obsolete SDK tools package.
- Rust compile fix: ui_theme::color::with_alpha is public so the separate Android crate can call it.
- Rust compile fix: documents.rs borrows `name` for dir.join(&name), so it remains available for the import notice.

APPLY
1. Extract this ZIP into the repository root (the same folder as Cargo.toml).
2. Allow the included files to overwrite files with matching paths.
3. Review `git diff`.
4. Commit and push the changes to the `android` branch.
5. Run the Android Build APK workflow again using workflow_dispatch.

NOTE
This is a source overlay, not a prebuilt APK. The two compiler errors reported in
GitHub Actions are addressed in this archive, but a successful APK build still
needs to be confirmed by a new Actions run.
