HORIZON ANDROID — COMPLETE CHANGES OVERLAY

This is a small overlay, not a full source archive. It is prepared for the
CihuyAkz/horizon-android-plus `android` branch after commit
86e881b6940f698f75a4e2a2551f594073f6194f. It includes the previously omitted
shader-import pipeline files as well as the UI, joystick and workflow changes.

CONTENTS
- crates/horizon-client/src/content_files.rs
  Validates classic shader archives and safely extracts a root shaders/ folder
  or one wrapper directory while enforcing archive limits and path checks.
- crates/horizon-client/src/android/documents.rs
  Installs extracted classic shader packs as folders and supports folder conflicts.
- crates/horizon-client/src/android/catalog.rs
  Shows extracted shader folders in Installed and permits removing those folders.
- crates/horizon-client/src/android/launcher_pages.rs
  Rounded panels, refreshed buttons/navigation, subtle animated header accents,
  download progress visuals and the requested creator credits. It also preserves
  the branch's latest diagnostics controls.
- crates/horizon-client-render/src/touch.rs
  Enlarges the inner joystick knob without enlarging the outer frame.
- .github/workflows/android-build.yml
  Uses platform-tools only in setup-android so sdkmanager does not request the
  obsolete `tools` package.
- docs/android.md
  Build workflow guidance.

APPLY
1. Extract this ZIP into the repository root, preserving the folder paths.
2. Replace the matching files when prompted.
3. Review `git diff` before committing.
4. Commit and push the changes to the `android` branch.
5. Start a NEW Actions run after the commit is visible on GitHub.

IMPORTANT
GitHub Actions compiles the commit checked out by the workflow. Files copied
only on your phone/PC but not committed and pushed are not included in that APK.
The earlier changes-only archive was incomplete: it omitted content_files.rs,
documents.rs, catalog.rs and touch.rs. This archive includes them.

The extraction regression test is included but has not been run here. A successful
APK build and on-device shader-pack test still need to be confirmed by GitHub Actions
and the Android app.
