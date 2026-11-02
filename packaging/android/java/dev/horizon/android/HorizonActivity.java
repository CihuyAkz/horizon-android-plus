package dev.horizon.android;

import android.app.Activity;
import android.app.AlarmManager;
import android.app.Dialog;
import android.app.PendingIntent;
import android.content.Intent;
import android.graphics.Color;
import android.graphics.drawable.ColorDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.view.Gravity;
import android.view.View;
import android.view.Window;
import android.view.WindowManager;
import android.webkit.JavascriptInterface;
import android.webkit.ValueCallback;
import android.webkit.WebChromeClient;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import org.json.JSONObject;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.io.RandomAccessFile;
import java.security.MessageDigest;
import java.util.Locale;
import java.util.zip.ZipEntry;
import java.util.zip.ZipInputStream;

/** Native game host with the HTML launcher presented above the Rust surface. */
public final class HorizonActivity extends android.app.NativeActivity {
    private static final int REQUEST_DOCUMENT_IMPORT = 4101;
    private static final int REQUEST_WEB_FILE = 4102;
    private static final int REQUEST_RUNTIME_IMPORT = 4103;
    private static final long MAX_RUNTIME_LIBRARY_BYTES = 384L * 1024L * 1024L;
    private static final long MAX_RUNTIME_ARCHIVE_BYTES = 512L * 1024L * 1024L;

    static { System.loadLibrary("horizon_android"); }

    private native void nativeDocumentResult(int request, int result, String uri);
    private native void nativeWebCommand(int action, String payload);
    private native String nativeWebStatus();

    private WebView webView;
    private Dialog launcherDialog;
    private ValueCallback<Uri[]> webFileCallback;

    @Override protected void onCreate(Bundle savedInstanceState) {
        // The NativeActivity loader chooses a runtime before super.onCreate loads the engine.
        prepareRuntimeLibrary();
        super.onCreate(savedInstanceState);
        createHtmlLauncher();
    }

    private void createHtmlLauncher() {
        webView = new WebView(this);
        webView.setBackgroundColor(Color.rgb(16, 17, 22));
        webView.setFocusable(true);
        webView.setFocusableInTouchMode(true);

        WebSettings settings = webView.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(true);
        settings.setAllowFileAccess(true);
        settings.setAllowFileAccessFromFileURLs(false);
        settings.setAllowUniversalAccessFromFileURLs(false);
        settings.setAllowContentAccess(false);
        settings.setJavaScriptCanOpenWindowsAutomatically(false);
        settings.setSupportMultipleWindows(false);
        settings.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        settings.setSafeBrowsingEnabled(true);

        // The JavaScript interface is exposed only to the bundled, trusted launcher page.
        webView.addJavascriptInterface(new HorizonBridge(), "HorizonAndroid");
        webView.setWebViewClient(new WebViewClient() {
            @Override public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                return routeUrl(request.getUrl());
            }

            @Override public boolean shouldOverrideUrlLoading(WebView view, String url) {
                return routeUrl(Uri.parse(url));
            }

            private boolean routeUrl(Uri uri) {
                String value = uri.toString();
                if (value.startsWith("file:///android_asset/")) return false;
                if ("https".equalsIgnoreCase(uri.getScheme())) {
                    try { startActivity(new Intent(Intent.ACTION_VIEW, uri)); } catch (Exception ignored) { }
                }
                return true;
            }
        });
        webView.setWebChromeClient(new WebChromeClient() {
            @Override public boolean onShowFileChooser(WebView view,
                    ValueCallback<Uri[]> callback, FileChooserParams params) {
                if (webFileCallback != null) webFileCallback.onReceiveValue(null);
                webFileCallback = callback;
                try {
                    Intent intent = params.createIntent();
                    startActivityForResult(intent, REQUEST_WEB_FILE);
                    return true;
                } catch (Exception error) {
                    webFileCallback = null;
                    callback.onReceiveValue(null);
                    return false;
                }
            }
        });

        // NativeActivity renders into the Activity's main window. Keep the HTML
        // launcher in a separate attached window so the continuously rendered
        // Rust launcher surface cannot paint over the WebView.
        launcherDialog = new Dialog(this, android.R.style.Theme_Material_NoActionBar);
        launcherDialog.setContentView(webView);
        launcherDialog.setCancelable(false);
        launcherDialog.setCanceledOnTouchOutside(false);
        launcherDialog.show();
        configureLauncherDialogWindow();
        webView.loadUrl("file:///android_asset/Horizon-Android-Plus.html");
    }

    private static String readUtf8(InputStream input, int limit) throws java.io.IOException {
        ByteArrayOutputStream output = new ByteArrayOutputStream();
        byte[] buffer = new byte[8192];
        int total = 0;
        int count;
        while ((count = input.read(buffer)) != -1) {
            total += count;
            if (total > limit) throw new java.io.IOException("Metadata file is too large");
            output.write(buffer, 0, count);
        }
        return output.toString(java.nio.charset.StandardCharsets.UTF_8.name());
    }

    private static void writeText(File file, String value) throws java.io.IOException {
        File parent = file.getParentFile();
        if (parent != null && !parent.isDirectory() && !parent.mkdirs()) {
            throw new java.io.IOException("Could not create " + parent);
        }
        File partial = new File(file.getPath() + ".partial");
        try (FileOutputStream output = new FileOutputStream(partial)) {
            output.write(value.getBytes(java.nio.charset.StandardCharsets.UTF_8));
            output.getFD().sync();
        }
        if (file.exists() && !file.delete()) throw new java.io.IOException("Could not replace " + file);
        if (!partial.renameTo(file)) throw new java.io.IOException("Could not save " + file);
    }

    private static void copyFile(File source, File target) throws java.io.IOException {
        File parent = target.getParentFile();
        if (parent != null && !parent.isDirectory() && !parent.mkdirs()) {
            throw new java.io.IOException("Could not create " + parent);
        }
        File partial = new File(target.getPath() + ".partial");
        try (InputStream input = new FileInputStream(source);
             FileOutputStream output = new FileOutputStream(partial)) {
            byte[] buffer = new byte[1024 * 1024];
            int count;
            while ((count = input.read(buffer)) != -1) output.write(buffer, 0, count);
            output.getFD().sync();
        }
        if (target.exists() && !target.delete()) throw new java.io.IOException("Could not replace runtime library");
        if (!partial.renameTo(target)) throw new java.io.IOException("Could not install runtime library");
        if (!target.setReadable(true, false) || !target.setReadOnly()) {
            throw new java.io.IOException("Could not mark runtime library read-only");
        }
    }

    private static String sha256File(File file) throws Exception {
        MessageDigest digest = MessageDigest.getInstance("SHA-256");
        try (InputStream input = new FileInputStream(file)) {
            byte[] buffer = new byte[1024 * 1024];
            int count;
            while ((count = input.read(buffer)) != -1) digest.update(buffer, 0, count);
        }
        StringBuilder result = new StringBuilder(64);
        for (byte value : digest.digest()) result.append(String.format(Locale.ROOT, "%02x", value & 0xff));
        return result.toString();
    }

    private static boolean validRuntimeId(String id) {
        return id != null && id.matches("native-[A-Za-z0-9._-]{7,89}") && !id.contains("..");
    }

    private static boolean verifyRuntimeElf(File file, String abi) throws java.io.IOException {
        try (RandomAccessFile input = new RandomAccessFile(file, "r")) {
            byte[] header = new byte[20];
            input.readFully(header);
            if (header[0] != 0x7f || header[1] != 'E' || header[2] != 'L' || header[3] != 'F') return false;
            boolean little = header[5] == 1;
            if (!little && header[5] != 2) return false;
            int machine = little ? ((header[19] & 0xff) << 8) | (header[18] & 0xff)
                    : ((header[18] & 0xff) << 8) | (header[19] & 0xff);
            switch (abi) {
                case "arm64-v8a": return machine == 183;
                case "armeabi-v7a": return machine == 40;
                case "x86": return machine == 3;
                case "x86_64": return machine == 62;
                default: return false;
            }
        }
    }

    private JSONObject packagedRuntimeManifest() throws Exception {
        try (InputStream input = getAssets().open("horizon-runtime.json")) {
            return new JSONObject(readUtf8(input, 16 * 1024));
        }
    }

    private boolean verifyRuntimeDirectory(File directory, String id, String abi, String minecraftVersion) throws Exception {
        File library = new File(directory, "libhorizon_runtime.so");
        File metadata = new File(directory, "runtime.json");
        if (!library.isFile() || !metadata.isFile() || !verifyRuntimeElf(library, abi)) return false;
        JSONObject manifest;
        try (FileInputStream input = new FileInputStream(metadata)) {
            manifest = new JSONObject(readUtf8(input, 16 * 1024));
        }
        return manifest.optInt("schema") == 1
                && id.equals(manifest.optString("id"))
                && abi.equals(manifest.optString("abi"))
                && minecraftVersion.equals(manifest.optString("minecraftVersion"))
                && "libhorizon_runtime.so".equals(manifest.optString("library"))
                && sha256File(library).equalsIgnoreCase(manifest.optString("sha256"));
    }

    private void prepareRuntimeLibrary() {
        try {
            JSONObject manifest = packagedRuntimeManifest();
            String defaultId = manifest.getString("id");
            String abi = manifest.getString("abi");
            String expectedHash = manifest.getString("sha256");
            if (!validRuntimeId(defaultId)) throw new java.io.IOException("Invalid APK runtime ID");

            File root = new File(getFilesDir(), "horizon-runtimes");
            File defaultDir = new File(root, defaultId);
            File installedLibrary = new File(defaultDir, "libhorizon_runtime.so");
            File packagedLibrary = new File(getApplicationInfo().nativeLibraryDir, "libhorizon_runtime.so");
            if (!installedLibrary.isFile() || !expectedHash.equalsIgnoreCase(sha256File(installedLibrary))) {
                copyFile(packagedLibrary, installedLibrary);
            }
            if (!expectedHash.equalsIgnoreCase(sha256File(installedLibrary)) || !verifyRuntimeElf(installedLibrary, abi)) {
                throw new java.io.IOException("The APK's Horizon runtime failed its integrity or ABI check");
            }
            writeText(new File(defaultDir, "runtime.json"), manifest.toString(2));
            writeText(new File(getFilesDir(), "horizon-runtime-default.txt"), defaultId + "\n");

            String followId = defaultId;
            File followFile = new File(getFilesDir(), "horizon-runtime-follow.txt");
            if (followFile.isFile()) {
                try (FileInputStream input = new FileInputStream(followFile)) {
                    String followed = readUtf8(input, 256).trim();
                    if (validRuntimeId(followed)
                            && verifyRuntimeDirectory(new File(root, followed), followed, abi,
                                    manifest.getString("minecraftVersion"))) {
                        followId = followed;
                    }
                } catch (Exception followError) {
                    Log.w("Horizon", "Could not read the latest-follow runtime; using the APK build", followError);
                }
            }
            String selected = followId;
            File configFile = new File(getFilesDir(), "android-launcher.json");
            if (configFile.isFile()) {
                try (FileInputStream input = new FileInputStream(configFile)) {
                    JSONObject config = new JSONObject(readUtf8(input, 1024 * 1024));
                    String activeId = config.optString("selected_instance", "default");
                    org.json.JSONArray instances = config.optJSONArray("instances");
                    if (instances != null) {
                        for (int i = 0; i < instances.length(); i++) {
                            JSONObject instance = instances.optJSONObject(i);
                            if (instance != null && activeId.equals(instance.optString("id"))) {
                                String pin = instance.optString("version", "");
                                if (validRuntimeId(pin)) selected = pin;
                                break;
                            }
                        }
                    }
                } catch (Exception parseError) {
                    Log.w("Horizon", "Could not read selected instance runtime; using latest available build", parseError);
                    selected = followId;
                }
            }
            boolean selectedValid = selected.equals(defaultId)
                    || verifyRuntimeDirectory(new File(root, selected), selected, abi,
                            manifest.getString("minecraftVersion"));
            if (!selectedValid) {
                Log.w("Horizon", "Selected build is missing or invalid; reverting to the latest available build: " + selected);
                selected = followId;
                if (!selected.equals(defaultId)
                        && !verifyRuntimeDirectory(new File(root, selected), selected, abi,
                                manifest.getString("minecraftVersion"))) {
                    selected = defaultId;
                }
            }
            writeText(new File(getFilesDir(), "horizon-runtime-selection.txt"), selected + "\n");
        } catch (Exception error) {
            Log.e("Horizon", "Preparing the native runtime failed", error);
        }
    }

    private String currentRuntimeAbi() throws Exception {
        return packagedRuntimeManifest().getString("abi");
    }

    private void reportRuntimeImport(String message, boolean success, String id) {
        runOnUiThread(() -> {
            WebView target = webView;
            if (target != null) target.evaluateJavascript(
                    "if(typeof toast==='function')toast(" + org.json.JSONObject.quote(message) + ");", null);
            if (success && validRuntimeId(id)) nativeWebCommand(16, id);
        });
    }

    private void importRuntimeArchive(Uri uri) {
        Thread worker = new Thread(() -> {
            try {
                InputStream opened = getContentResolver().openInputStream(uri);
                if (opened == null) throw new java.io.IOException("The selected file cannot be opened");
                installRuntimeArchive(opened, false);
            } catch (Exception error) {
                Log.e("Horizon", "Runtime import failed", error);
                reportRuntimeImport("Runtime import failed: " + error.getMessage(), false, "");
            }
        }, "HorizonRuntimeImport");
        worker.setDaemon(true);
        worker.start();
    }

    private void downloadOfficialRuntime(String address, boolean followLatest) {
        Thread worker = new Thread(() -> {
            File archive = null;
            java.net.HttpURLConnection connection = null;
            try {
                java.net.URL source = new java.net.URL(address);
                if (!"https".equalsIgnoreCase(source.getProtocol())
                        || !"github.com".equalsIgnoreCase(source.getHost())
                        || !source.getPath().startsWith("/horizon-rust/horizon/releases/download/")) {
                    throw new java.io.IOException("The Nightly download URL is not an official Horizon release asset");
                }
                File cacheRoot = new File(getCacheDir(), "horizon-runtime-downloads");
                if (!cacheRoot.isDirectory() && !cacheRoot.mkdirs()) {
                    throw new java.io.IOException("Could not create the runtime download cache");
                }
                archive = File.createTempFile("official-nightly-", ".hruntime.zip", cacheRoot);
                connection = (java.net.HttpURLConnection) source.openConnection();
                connection.setConnectTimeout(15000);
                connection.setReadTimeout(60000);
                connection.setInstanceFollowRedirects(true);
                connection.setRequestProperty("User-Agent", "Horizon-Android-Plus");
                connection.setRequestProperty("Accept", "application/octet-stream");
                int response = connection.getResponseCode();
                if (response != 200) throw new java.io.IOException("Nightly download returned HTTP " + response);
                String finalHost = connection.getURL().getHost();
                if (!"github.com".equalsIgnoreCase(finalHost)
                        && !"release-assets.githubusercontent.com".equalsIgnoreCase(finalHost)) {
                    throw new java.io.IOException("The Nightly download redirected to an untrusted host");
                }
                long declared = connection.getContentLengthLong();
                if (declared > MAX_RUNTIME_ARCHIVE_BYTES) {
                    throw new java.io.IOException("The Android runtime archive exceeds the download limit");
                }
                try (InputStream input = connection.getInputStream();
                     FileOutputStream output = new FileOutputStream(archive)) {
                    byte[] buffer = new byte[64 * 1024];
                    long total = 0;
                    int count;
                    while ((count = input.read(buffer)) != -1) {
                        total += count;
                        if (total > MAX_RUNTIME_ARCHIVE_BYTES) {
                            throw new java.io.IOException("The Android runtime archive exceeds the download limit");
                        }
                        output.write(buffer, 0, count);
                    }
                    output.getFD().sync();
                }
                try (InputStream input = new FileInputStream(archive)) {
                    installRuntimeArchive(input, followLatest);
                }
            } catch (Exception error) {
                Log.e("Horizon", "Official Nightly download failed", error);
                reportRuntimeImport("Nightly installation failed: " + error.getMessage(), false, "");
            } finally {
                if (connection != null) connection.disconnect();
                if (archive != null) archive.delete();
            }
        }, "HorizonOfficialNightly");
        worker.setDaemon(true);
        worker.start();
    }

    /** Validate the archive and install only its ABI-matched native library and manifest SHA-256. */
    private void installRuntimeArchive(InputStream opened, boolean followLatest) throws Exception {
        File temporaryLibrary = null;
        try {
            String abi = currentRuntimeAbi();
            String expectedMinecraftVersion = packagedRuntimeManifest().getString("minecraftVersion");
            String manifestText = null;
            File cacheRoot = new File(getCacheDir(), "horizon-runtime-imports");
            if (!cacheRoot.isDirectory() && !cacheRoot.mkdirs()) throw new java.io.IOException("Could not create import cache");
            temporaryLibrary = File.createTempFile("runtime-", ".so", cacheRoot);
            boolean foundLibrary = false;
            try (ZipInputStream zip = new ZipInputStream(opened)) {
                ZipEntry entry;
                byte[] buffer = new byte[1024 * 1024];
                while ((entry = zip.getNextEntry()) != null) {
                    if (entry.isDirectory()) continue;
                    String name = entry.getName();
                    if ("runtime.json".equals(name)) {
                        if (manifestText != null) throw new java.io.IOException("Duplicate runtime metadata");
                        ByteArrayOutputStream metadata = new ByteArrayOutputStream();
                        int total = 0;
                        int count;
                        while ((count = zip.read(buffer, 0, Math.min(buffer.length, 8192))) != -1) {
                            total += count;
                            if (total > 16 * 1024) throw new java.io.IOException("Runtime metadata is too large");
                            metadata.write(buffer, 0, count);
                        }
                        manifestText = metadata.toString(java.nio.charset.StandardCharsets.UTF_8.name());
                    } else if ("libhorizon_runtime.so".equals(name)) {
                        if (foundLibrary) throw new java.io.IOException("Duplicate runtime library");
                        long total = 0;
                        try (FileOutputStream output = new FileOutputStream(temporaryLibrary)) {
                            int count;
                            while ((count = zip.read(buffer)) != -1) {
                                total += count;
                                if (total > MAX_RUNTIME_LIBRARY_BYTES) throw new java.io.IOException("Runtime library exceeds the import limit");
                                output.write(buffer, 0, count);
                            }
                            output.getFD().sync();
                        }
                        foundLibrary = true;
                    } else {
                        throw new java.io.IOException("Unsupported file inside runtime bundle: " + name);
                    }
                }
            }
            if (manifestText == null || !foundLibrary) throw new java.io.IOException("This ZIP is not a Horizon Android runtime bundle");
            JSONObject manifest = new JSONObject(manifestText);
            String id = manifest.getString("id");
            if (manifest.optInt("schema") != 1 || !validRuntimeId(id)) throw new java.io.IOException("Unsupported Horizon runtime metadata");
            if (!abi.equals(manifest.optString("abi"))) throw new java.io.IOException("This build is for " + manifest.optString("abi") + ", but this app requires " + abi);
            if (!expectedMinecraftVersion.equals(manifest.optString("minecraftVersion"))) throw new java.io.IOException("This build targets Minecraft " + manifest.optString("minecraftVersion") + ", not " + expectedMinecraftVersion);
            if (!"libhorizon_runtime.so".equals(manifest.optString("library"))) throw new java.io.IOException("Unexpected runtime library name");
            if (!verifyRuntimeElf(temporaryLibrary, abi)) throw new java.io.IOException("The runtime binary does not match its declared Android ABI");
            String actualHash = sha256File(temporaryLibrary);
            if (!actualHash.equalsIgnoreCase(manifest.optString("sha256"))) throw new java.io.IOException("Runtime checksum mismatch");

            File runtimeRoot = new File(getFilesDir(), "horizon-runtimes");
            File runtimeDir = new File(runtimeRoot, id);
            if (!runtimeRoot.isDirectory() && !runtimeRoot.mkdirs()) throw new java.io.IOException("Could not create runtime directory");
            if (!runtimeDir.isDirectory() && !runtimeDir.mkdirs()) throw new java.io.IOException("Could not create runtime version folder");
            File target = new File(runtimeDir, "libhorizon_runtime.so");
            if (target.exists() && !target.delete()) throw new java.io.IOException("Could not replace existing runtime");
            if (!temporaryLibrary.renameTo(target)) throw new java.io.IOException("Could not move the imported runtime into place");
            temporaryLibrary = null;
            if (!target.setReadable(true, false) || !target.setReadOnly()) {
                throw new java.io.IOException("Could not mark imported runtime read-only");
            }
            writeText(new File(runtimeDir, "runtime.json"), manifest.toString(2));
            if (followLatest) writeText(new File(getFilesDir(), "horizon-runtime-follow.txt"), id + "\n");
            reportRuntimeImport((followLatest ? "Installed latest official Horizon Nightly " : "Imported Horizon build ") + id, true, id);
        } finally {
            if (temporaryLibrary != null) temporaryLibrary.delete();
        }
    }

    private void restartHorizon() {
        Intent launch = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (launch == null) {
            reportRuntimeImport("Could not restart Horizon automatically. Close and reopen the app.", false, "");
            return;
        }
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent pending = PendingIntent.getActivity(this, 4104, launch,
                PendingIntent.FLAG_CANCEL_CURRENT | PendingIntent.FLAG_IMMUTABLE);
        AlarmManager alarm = (AlarmManager) getSystemService(ALARM_SERVICE);
        if (alarm != null) alarm.set(AlarmManager.ELAPSED_REALTIME_WAKEUP,
                SystemClock.elapsedRealtime() + 900, pending);
        new Handler(Looper.getMainLooper()).postDelayed(() -> {
            try { finishAffinity(); } catch (Exception ignored) { }
            android.os.Process.killProcess(android.os.Process.myPid());
        }, 250);
    }

    private void configureLauncherDialogWindow() {
        if (launcherDialog == null) return;
        Window window = launcherDialog.getWindow();
        if (window == null) return;
        Window hostWindow = getWindow();
        int inheritedFlags = WindowManager.LayoutParams.FLAG_FULLSCREEN
                | WindowManager.LayoutParams.FLAG_LAYOUT_IN_SCREEN
                | WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS;
        window.setBackgroundDrawable(new ColorDrawable(Color.TRANSPARENT));
        window.clearFlags(WindowManager.LayoutParams.FLAG_DIM_BEHIND | inheritedFlags);
        window.addFlags(hostWindow.getAttributes().flags & inheritedFlags);
        window.setStatusBarColor(hostWindow.getStatusBarColor());
        window.setNavigationBarColor(hostWindow.getNavigationBarColor());
        window.setGravity(Gravity.TOP | Gravity.START);
        window.setLayout(WindowManager.LayoutParams.MATCH_PARENT,
                WindowManager.LayoutParams.MATCH_PARENT);
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        window.getDecorView().setSystemUiVisibility(
                hostWindow.getDecorView().getSystemUiVisibility());
    }

    /** Called from Rust when the actual game takes over or returns to the launcher. */
    public void setWebLauncherVisible(boolean visible) {
        runOnUiThread(() -> {
            if (launcherDialog == null || webView == null) return;
            if (visible) {
                webView.setVisibility(View.VISIBLE);
                if (!launcherDialog.isShowing()) launcherDialog.show();
                configureLauncherDialogWindow();
                webView.bringToFront();
                webView.requestFocus();
            } else if (launcherDialog.isShowing()) {
                launcherDialog.dismiss();
            }
        });
    }

    private String fetchHttpsText(String address) throws java.io.IOException {
        java.net.HttpURLConnection connection = (java.net.HttpURLConnection)
                new java.net.URL(address).openConnection();
        connection.setRequestMethod("GET");
        connection.setConnectTimeout(8000);
        connection.setReadTimeout(8000);
        connection.setInstanceFollowRedirects(true);
        connection.setRequestProperty("User-Agent", "Horizon-Android-Plus");
        connection.setRequestProperty("Accept", "application/vnd.github+json");
        connection.setRequestProperty("X-GitHub-Api-Version", "2022-11-28");
        try {
            int status = connection.getResponseCode();
            if (status < 200 || status >= 300) return "";
            try (java.io.BufferedReader reader = new java.io.BufferedReader(
                    new java.io.InputStreamReader(connection.getInputStream(), java.nio.charset.StandardCharsets.UTF_8))) {
                StringBuilder text = new StringBuilder();
                String line;
                while ((line = reader.readLine()) != null) text.append(line).append('\n');
                return text.toString();
            }
        } finally {
            connection.disconnect();
        }
    }

    private String releaseVersionForTag(String tag) {
        try {
            String encodedTag = android.net.Uri.encode(tag);
            String gradle = fetchHttpsText("https://raw.githubusercontent.com/CihuyAkz/horizon-android-plus/"
                    + encodedTag + "/packaging/android/build.gradle");
            java.util.regex.Matcher matcher = java.util.regex.Pattern
                    .compile("(?m)^\\s*versionName\\s+['\\\"]([^'\\\"]+)['\\\"]")
                    .matcher(gradle);
            if (matcher.find()) return matcher.group(1).trim();
        } catch (Exception ignored) { }
        java.util.regex.Matcher tagMatcher = java.util.regex.Pattern
                .compile("(?i)^v?(\\d+\\.\\d+\\.\\d+(?:-[0-9a-z.-]+)?(?:\\+[0-9a-z.-]+)?)$")
                .matcher(tag == null ? "" : tag.trim());
        return tagMatcher.matches() ? tagMatcher.group(1) : "";
    }

    private String fetchLatestPublicReleasePayload() {
        org.json.JSONObject result = new org.json.JSONObject();
        try {
            String response = fetchHttpsText(
                    "https://api.github.com/repos/CihuyAkz/horizon-android-plus/releases?per_page=30");
            if (response.isEmpty()) return result.put("checked", false).toString();
            org.json.JSONArray releases = new org.json.JSONArray(response);
            for (int index = 0; index < releases.length(); index++) {
                org.json.JSONObject item = releases.optJSONObject(index);
                if (item == null || item.optBoolean("draft", false)) continue;
                org.json.JSONArray assets = item.optJSONArray("assets");
                boolean hasApk = false;
                if (assets != null) {
                    for (int assetIndex = 0; assetIndex < assets.length(); assetIndex++) {
                        org.json.JSONObject asset = assets.optJSONObject(assetIndex);
                        if (asset == null) continue;
                        String assetName = asset.optString("name", "");
                        if (assetName.toLowerCase(java.util.Locale.ROOT).endsWith(".apk")
                                && "uploaded".equalsIgnoreCase(asset.optString("state", "uploaded"))) {
                            hasApk = true;
                            break;
                        }
                    }
                }
                if (!hasApk) continue;
                String tag = item.optString("tag_name", "");
                String version = releaseVersionForTag(tag);
                if (version.isEmpty()) continue;
                org.json.JSONObject release = new org.json.JSONObject();
                release.put("name", item.optString("name", tag));
                release.put("tag", tag);
                release.put("version", version);
                release.put("body", item.optString("body", ""));
                release.put("url", item.optString("html_url",
                        "https://github.com/CihuyAkz/horizon-android-plus/releases"));
                release.put("prerelease", item.optBoolean("prerelease", false));
                result.put("checked", true).put("release", release);
                return result.toString();
            }
            result.put("checked", true);
        } catch (Exception ignored) {
            try { result.put("checked", false); } catch (org.json.JSONException ignoredAgain) { }
        }
        return result.toString();
    }

    private final class HorizonBridge {
        @JavascriptInterface public void command(int action, String payload) {
            nativeWebCommand(action, payload == null ? "" : payload);
        }

        @JavascriptInterface public String status() {
            String result = nativeWebStatus();
            return result == null ? "{}" : result;
        }

        @JavascriptInterface public void importRuntime() {
            runOnUiThread(() -> {
                Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                intent.addCategory(Intent.CATEGORY_OPENABLE);
                intent.setType("application/zip");
                try { startActivityForResult(intent, REQUEST_RUNTIME_IMPORT); }
                catch (Exception error) { reportRuntimeImport("No file picker is available", false, ""); }
            });
        }

        @JavascriptInterface public void downloadRuntime(String address, boolean followLatest) {
            downloadOfficialRuntime(address == null ? "" : address, followLatest);
        }

        @JavascriptInterface public void restartApp() {
            runOnUiThread(HorizonActivity.this::restartHorizon);
        }

        @JavascriptInterface public String appVersion() {
            try {
                android.content.pm.PackageInfo info = HorizonActivity.this.getPackageManager()
                        .getPackageInfo(HorizonActivity.this.getPackageName(), 0);
                return info.versionName == null ? "" : info.versionName;
            } catch (android.content.pm.PackageManager.NameNotFoundException ignored) {
                return "";
            }
        }

        @JavascriptInterface public void checkLatestRelease() {
            Thread worker = new Thread(() -> {
                String payload = fetchLatestPublicReleasePayload();
                WebView target = webView;
                if (target == null) return;
                target.post(() -> target.evaluateJavascript(
                        "if(typeof window.onHorizonReleaseResult==='function')"
                                + "window.onHorizonReleaseResult(" + org.json.JSONObject.quote(payload) + ");",
                        null));
            }, "HorizonReleaseCheck");
            worker.setDaemon(true);
            worker.start();
        }
    }

    @Override protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == REQUEST_RUNTIME_IMPORT) {
            if (result == Activity.RESULT_OK && data != null && data.getData() != null) {
                importRuntimeArchive(data.getData());
            } else {
                reportRuntimeImport("Runtime import cancelled", false, "");
            }
            return;
        }
        if (request == REQUEST_DOCUMENT_IMPORT) {
            nativeDocumentResult(request, result,
                    data == null || data.getData() == null ? null : data.getData().toString());
            return;
        }
        if (request == REQUEST_WEB_FILE && webFileCallback != null) {
            Uri[] resultUris = null;
            if (result == Activity.RESULT_OK && data != null) {
                Uri uri = data.getData();
                if (uri != null) resultUris = new Uri[] { uri };
                if (uri != null) {
                    try { getContentResolver().takePersistableUriPermission(uri,
                            data.getFlags() & (Intent.FLAG_GRANT_READ_URI_PERMISSION
                                    | Intent.FLAG_GRANT_WRITE_URI_PERMISSION)); }
                    catch (SecurityException ignored) { }
                }
            }
            webFileCallback.onReceiveValue(resultUris);
            webFileCallback = null;
        }
    }

    @Override protected void onDestroy() {
        if (webFileCallback != null) {
            webFileCallback.onReceiveValue(null);
            webFileCallback = null;
        }
        if (launcherDialog != null) {
            if (launcherDialog.isShowing()) launcherDialog.dismiss();
            launcherDialog = null;
        }
        if (webView != null) {
            webView.removeJavascriptInterface("HorizonAndroid");
            webView.stopLoading();
            webView.destroy();
            webView = null;
        }
        super.onDestroy();
    }
}
