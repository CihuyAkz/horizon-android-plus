HORIZON ANDROID NATIVE UI UPDATE

Replace these three files in the source tree with the matching files in this package:

- crates/horizon-client/src/ui_theme.rs
- crates/horizon-client/src/android/launcher_pages.rs
- crates/horizon-client/src/android/catalog.rs

WHAT CHANGED

- Replaced the old horizontal page strip with a native sidebar layout: Home, Downloads categories, Installed Library, Settings, and native Account.
- Restyled native buttons, cards, search areas, library entries, and settings rows with the dark/violet visual language from Horizon-Android-v9.html.
- Kept the actual native Play installer/game handoff, Microsoft sign-in, Android document picker, local add-on management, settings persistence, and diagnostics.
- Removed demo/sample/simulation-only UI concepts from the port. The HTML prototype itself is not bundled or loaded by Android.
- Removed the arbitrary HTTPS .hmod download shortcut from the Mods catalogue. Local .hmod imports remain available through Android's native file picker.

DOWNLOAD PROVIDER POLICY

- Mods: Horizon Registry only.
- Resource Packs: Modrinth only.
- Shaders: Modrinth only.
- CurseForge and other provider choices are not shown or used by these Android catalogue routes.
- Provider validation is enforced before installation, not just in the UI.

BUILD / VALIDATION

This package was checked for balanced Rust delimiters and internal Action/Page references. A Rust compiler/Cargo is not installed in the editing environment, so cargo fmt, cargo check, Android build, and on-device rendering were not run. Run the repository's Android build workflow before installing the APK.

The unified diff is included as horizon-android-native-ui.patch. The complete replacement Rust source files are included at their repository paths.
