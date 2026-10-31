HORIZON ANDROID — CHANGES ONLY

Isi ZIP ini hanya file yang berubah sejak source awal, bukan full source.

CARA PASANG
1. Extract ZIP ini.
2. Salin seluruh isi folder hasil extract ke root repository/source Horizon Android kamu.
3. Izinkan overwrite/replace file jika diminta.
4. Commit dan push ke GitHub.
5. Jalankan GitHub Actions > Build Android APK.

FILE YANG DIBAWA:
- docs/android.md
- .github/workflows/android-build.yml
- crates/horizon-client/src/android/launcher_pages.rs

CATATAN
- File launcher_pages.rs mencakup perubahan impor/ekstraksi shader ZIP, daftar Installed, UI/animasi, kredit, dan ukuran knob joystick yang diminta dalam rangkaian update.
- Workflow android-build.yml disertakan untuk compile melalui GitHub Actions.
- File docs/android.md diperbarui dengan petunjuk workflow.
- ZIP ini adalah overlay perubahan; tetap membutuhkan source dasar/repository Horizon Android yang sesuai.
