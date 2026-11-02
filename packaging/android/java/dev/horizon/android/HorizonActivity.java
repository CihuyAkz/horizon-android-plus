package dev.horizon.android;

import android.app.Activity;
import android.app.Dialog;
import android.app.DownloadManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.graphics.Color;
import android.graphics.drawable.ColorDrawable;
import android.net.Uri;
import android.os.Bundle;
import android.os.Build;
import android.os.Environment;
import android.provider.Settings;
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
import android.widget.Toast;

/** Native game host with the HTML launcher presented above the Rust surface. */
public final class HorizonActivity extends android.app.NativeActivity {
    private static final int REQUEST_DOCUMENT_IMPORT = 4101;
    private static final int REQUEST_WEB_FILE = 4102;
    private static final int REQUEST_INSTALL_PERMISSION = 4103;
    private static final String VERSION_PREFS = "horizon-version-installer";
    private static final String VERSION_DOWNLOAD_ID = "pending-download-id";
    private static final String APK_MIME_TYPE = "application/vnd.android.package-archive";
    private static final String RELEASE_ASSET_PREFIX = "/CihuyAkz/horizon-android-plus/releases/download/";

    static { System.loadLibrary("horizon_android"); }

    private native void nativeDocumentResult(int request, int result, String uri);
    private native void nativeWebCommand(int action, String payload);
    private native String nativeWebStatus();

    private WebView webView;
    private Dialog launcherDialog;
    private ValueCallback<Uri[]> webFileCallback;
    private BroadcastReceiver versionDownloadReceiver;
    private long pendingVersionDownloadId = -1L;
    private String pendingVersionUrl;
    private String pendingVersionFilename;
    private boolean installPermissionInFlight;

    @Override protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        pendingVersionDownloadId = getSharedPreferences(VERSION_PREFS, MODE_PRIVATE)
                .getLong(VERSION_DOWNLOAD_ID, -1L);
        registerVersionDownloadReceiver();
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

    private String releaseVersion(org.json.JSONObject release) {
        String tag = release.optString("tag_name", "").trim();
        String title = release.optString("name", "").trim();
        java.util.regex.Pattern pattern = java.util.regex.Pattern.compile(
                "(?i)(\\d+\\.\\d+\\.\\d+(?:-[0-9a-z.-]+)?)");
        java.util.regex.Matcher matcher = pattern.matcher(tag);
        if (matcher.find()) return matcher.group(1);
        matcher = pattern.matcher(title);
        return matcher.find() ? matcher.group(1) : (tag.isEmpty() ? title : tag);
    }

    private boolean assetMatchesAbi(String filename, String abi) {
        String name = filename.toLowerCase(java.util.Locale.ROOT);
        if (name.contains("universal") || name.contains("noarch")) return true;
        switch (abi) {
            case "arm64-v8a":
                return name.contains("arm64-v8a") || name.contains("aarch64")
                        || name.matches(".*(^|[-_.])arm64([-. _]|$).*" );
            case "armeabi-v7a":
                return name.contains("armeabi-v7a") || name.contains("armv7")
                        || name.contains("armv7a");
            case "x86_64":
                return name.contains("x86_64") || name.contains("x86-64")
                        || name.contains("amd64");
            case "x86":
                return !name.contains("x86_64") && !name.contains("x86-64")
                        && (name.contains("i686") || name.contains("i386")
                        || name.matches(".*(^|[-_.])x86([-. _]|$).*"));
            default:
                return false;
        }
    }

    private String deviceArchitecture() {
        return Build.SUPPORTED_ABIS.length > 0 ? Build.SUPPORTED_ABIS[0] : "unknown";
    }

    private org.json.JSONObject compatibleReleaseAsset(org.json.JSONArray assets) {
        if (assets == null) return null;
        String[] supported = Build.SUPPORTED_ABIS;
        for (String abi : supported) {
            for (int i = 0; i < assets.length(); i++) {
                org.json.JSONObject asset = assets.optJSONObject(i);
                if (asset != null && assetMatchesAbi(asset.optString("name", ""), abi)) {
                    try {
                        org.json.JSONObject selected = new org.json.JSONObject(asset.toString());
                        selected.put("abi", abi);
                        return selected;
                    } catch (org.json.JSONException ignored) { }
                }
            }
        }
        return null;
    }

    private String fetchHorizonVersionsPayload() {
        org.json.JSONObject result = new org.json.JSONObject();
        try {
            String response = fetchHttpsText(
                    "https://api.github.com/repos/CihuyAkz/horizon-android-plus/releases?per_page=100");
            if (response.isEmpty()) {
                result.put("checked", false);
                result.put("error", "GitHub did not return a release list. Check your connection and try again.");
                return result.toString();
            }
            org.json.JSONArray releases = new org.json.JSONArray(response);
            org.json.JSONArray versions = new org.json.JSONArray();
            for (int index = 0; index < releases.length(); index++) {
                org.json.JSONObject item = releases.optJSONObject(index);
                if (item == null || item.optBoolean("draft", false)) continue;
                org.json.JSONArray rawAssets = item.optJSONArray("assets");
                org.json.JSONArray apkAssets = new org.json.JSONArray();
                if (rawAssets != null) {
                    for (int assetIndex = 0; assetIndex < rawAssets.length(); assetIndex++) {
                        org.json.JSONObject asset = rawAssets.optJSONObject(assetIndex);
                        if (asset == null) continue;
                        String name = asset.optString("name", "");
                        String downloadUrl = asset.optString("browser_download_url", "");
                        if (!name.toLowerCase(java.util.Locale.ROOT).endsWith(".apk")
                                || !"uploaded".equalsIgnoreCase(asset.optString("state", "uploaded"))
                                || !isAllowedReleaseAsset(downloadUrl)) continue;
                        org.json.JSONObject apk = new org.json.JSONObject();
                        apk.put("name", name);
                        apk.put("url", downloadUrl);
                        apk.put("size", asset.optLong("size", 0L));
                        apkAssets.put(apk);
                    }
                }
                if (apkAssets.length() == 0) continue;

                org.json.JSONObject release = new org.json.JSONObject();
                String tag = item.optString("tag_name", "");
                String name = item.optString("name", tag);
                boolean prerelease = item.optBoolean("prerelease", false);
                boolean nightly = (name + " " + tag).toLowerCase(java.util.Locale.ROOT).contains("nightly");
                release.put("name", name);
                release.put("tag", tag);
                release.put("version", releaseVersion(item));
                release.put("body", item.optString("body", ""));
                release.put("url", item.optString("html_url",
                        "https://github.com/CihuyAkz/horizon-android-plus/releases"));
                release.put("publishedAt", item.optString("published_at", ""));
                release.put("prerelease", prerelease);
                release.put("channel", nightly ? "Nightly" : (prerelease ? "Pre-release" : "Release"));
                release.put("assets", apkAssets);
                org.json.JSONObject compatible = compatibleReleaseAsset(apkAssets);
                if (compatible != null) release.put("compatibleAsset", compatible);
                versions.put(release);
            }
            org.json.JSONArray deviceAbis = new org.json.JSONArray();
            for (String abi : Build.SUPPORTED_ABIS) deviceAbis.put(abi);
            result.put("checked", true);
            result.put("versions", versions);
            result.put("architecture", deviceArchitecture());
            result.put("deviceAbis", deviceAbis);
            result.put("count", versions.length());
        } catch (Exception error) {
            try {
                result.put("checked", false);
                result.put("error", "Could not read the Horizon release catalog. Please try again.");
            } catch (org.json.JSONException ignored) { }
        }
        return result.toString();
    }

    private boolean isAllowedReleaseAsset(String address) {
        try {
            Uri uri = Uri.parse(address);
            String path = uri.getPath();
            return "https".equalsIgnoreCase(uri.getScheme())
                    && "github.com".equalsIgnoreCase(uri.getHost())
                    && (uri.getPort() == -1 || uri.getPort() == 443)
                    && uri.getUserInfo() == null
                    && path != null && path.startsWith(RELEASE_ASSET_PREFIX)
                    && path.toLowerCase(java.util.Locale.ROOT).endsWith(".apk");
        } catch (Exception ignored) {
            return false;
        }
    }

    private void registerVersionDownloadReceiver() {
        versionDownloadReceiver = new BroadcastReceiver() {
            @Override public void onReceive(Context context, Intent intent) {
                if (!DownloadManager.ACTION_DOWNLOAD_COMPLETE.equals(intent.getAction())) return;
                long downloadId = intent.getLongExtra(DownloadManager.EXTRA_DOWNLOAD_ID, -1L);
                if (downloadId == pendingVersionDownloadId) handleVersionDownload(downloadId);
            }
        };
        IntentFilter filter = new IntentFilter(DownloadManager.ACTION_DOWNLOAD_COMPLETE);
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(versionDownloadReceiver, filter,
                    "android.permission.SEND_DOWNLOAD_COMPLETED_INTENTS", null,
                    Context.RECEIVER_EXPORTED);
        } else {
            registerReceiver(versionDownloadReceiver, filter,
                    "android.permission.SEND_DOWNLOAD_COMPLETED_INTENTS", null);
        }
    }

    private void savePendingVersionDownload(long downloadId) {
        pendingVersionDownloadId = downloadId;
        getSharedPreferences(VERSION_PREFS, MODE_PRIVATE).edit()
                .putLong(VERSION_DOWNLOAD_ID, downloadId).apply();
    }

    private void clearPendingVersionDownload() {
        pendingVersionDownloadId = -1L;
        getSharedPreferences(VERSION_PREFS, MODE_PRIVATE).edit()
                .remove(VERSION_DOWNLOAD_ID).apply();
    }

    private void requestVersionInstall(String address, String filename) {
        if (pendingVersionDownloadId >= 0L) {
            Toast.makeText(this, "A Horizon version download is already in progress.",
                    Toast.LENGTH_LONG).show();
            return;
        }
        if (!isAllowedReleaseAsset(address)) {
            Toast.makeText(this, "This APK download link is not trusted.", Toast.LENGTH_LONG).show();
            return;
        }
        String safeName = filename == null ? "horizon.apk"
                : filename.replaceAll("[^A-Za-z0-9._-]", "_");
        if (!safeName.toLowerCase(java.util.Locale.ROOT).endsWith(".apk")) {
            Toast.makeText(this, "Invalid APK filename.", Toast.LENGTH_LONG).show();
            return;
        }
        if (Build.VERSION.SDK_INT >= 26 && !getPackageManager().canRequestPackageInstalls()) {
            pendingVersionUrl = address;
            pendingVersionFilename = safeName;
            try {
                installPermissionInFlight = true;
                Intent settingsIntent = new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                        Uri.parse("package:" + getPackageName()));
                startActivityForResult(settingsIntent, REQUEST_INSTALL_PERMISSION);
            } catch (Exception error) {
                installPermissionInFlight = false;
                pendingVersionUrl = null;
                pendingVersionFilename = null;
                Toast.makeText(this, "Android could not open app-install permission settings.",
                        Toast.LENGTH_LONG).show();
            }
            return;
        }
        enqueueVersionDownload(address, safeName);
    }

    private void enqueueVersionDownload(String address, String filename) {
        try {
            DownloadManager manager = (DownloadManager) getSystemService(DOWNLOAD_SERVICE);
            if (manager == null) throw new IllegalStateException("Download service unavailable");
            String uniqueName = "Horizon-" + System.currentTimeMillis() + "-" + filename;
            DownloadManager.Request request = new DownloadManager.Request(Uri.parse(address));
            request.setTitle("Horizon version download");
            request.setDescription(filename);
            request.setMimeType(APK_MIME_TYPE);
            request.setNotificationVisibility(DownloadManager.Request.VISIBILITY_VISIBLE_NOTIFY_COMPLETED);
            request.setAllowedOverRoaming(false);
            request.setDestinationInExternalFilesDir(this, Environment.DIRECTORY_DOWNLOADS, uniqueName);
            savePendingVersionDownload(manager.enqueue(request));
            Toast.makeText(this, "Downloading " + filename + ". The installer will open when it is ready.",
                    Toast.LENGTH_LONG).show();
        } catch (Exception error) {
            Toast.makeText(this, "Could not start the APK download.", Toast.LENGTH_LONG).show();
        }
    }

    private void checkPendingVersionDownload() {
        if (pendingVersionDownloadId < 0L || installPermissionInFlight) return;
        handleVersionDownload(pendingVersionDownloadId);
    }

    private void handleVersionDownload(long downloadId) {
        DownloadManager manager = (DownloadManager) getSystemService(DOWNLOAD_SERVICE);
        if (manager == null) return;
        android.database.Cursor cursor = null;
        try {
            cursor = manager.query(new DownloadManager.Query().setFilterById(downloadId));
            if (cursor == null || !cursor.moveToFirst()) return;
            int statusColumn = cursor.getColumnIndex(DownloadManager.COLUMN_STATUS);
            int status = statusColumn >= 0 ? cursor.getInt(statusColumn) : -1;
            if (status == DownloadManager.STATUS_FAILED) {
                clearPendingVersionDownload();
                Toast.makeText(this, "The APK download failed. Try again from Versions.",
                        Toast.LENGTH_LONG).show();
            } else if (status == DownloadManager.STATUS_SUCCESSFUL) {
                installDownloadedVersion(manager, downloadId);
            }
        } catch (Exception ignored) {
        } finally {
            if (cursor != null) cursor.close();
        }
    }

    private void installDownloadedVersion(DownloadManager manager, long downloadId) {
        if (Build.VERSION.SDK_INT >= 26 && !getPackageManager().canRequestPackageInstalls()) {
            try {
                installPermissionInFlight = true;
                startActivityForResult(new Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                        Uri.parse("package:" + getPackageName())), REQUEST_INSTALL_PERMISSION);
            } catch (Exception ignored) {
                installPermissionInFlight = false;
            }
            return;
        }
        Uri apkUri = manager.getUriForDownloadedFile(downloadId);
        if (apkUri == null) {
            clearPendingVersionDownload();
            Toast.makeText(this, "The downloaded APK could not be opened.", Toast.LENGTH_LONG).show();
            return;
        }
        clearPendingVersionDownload();
        try {
            Intent installIntent = new Intent(Intent.ACTION_VIEW);
            installIntent.setDataAndType(apkUri, APK_MIME_TYPE);
            installIntent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
            startActivity(installIntent);
        } catch (Exception error) {
            Toast.makeText(this, "Android could not open the package installer.", Toast.LENGTH_LONG).show();
        }
    }

    private final class HorizonBridge {
        @JavascriptInterface public void command(int action, String payload) {
            nativeWebCommand(action, payload == null ? "" : payload);
        }

        @JavascriptInterface public String status() {
            String result = nativeWebStatus();
            return result == null ? "{}" : result;
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

        @JavascriptInterface public void checkVersions() {
            Thread worker = new Thread(() -> {
                String payload = fetchHorizonVersionsPayload();
                WebView target = webView;
                if (target == null) return;
                target.post(() -> target.evaluateJavascript(
                        "if(typeof window.onHorizonVersionsResult==='function')"
                                + "window.onHorizonVersionsResult(" + org.json.JSONObject.quote(payload) + ");",
                        null));
            }, "HorizonVersionsCheck");
            worker.setDaemon(true);
            worker.start();
        }

        @JavascriptInterface public void installVersion(String address, String filename) {
            runOnUiThread(() -> requestVersionInstall(address, filename));
        }
    }

    @Override protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        if (request == REQUEST_INSTALL_PERMISSION) {
            installPermissionInFlight = false;
            if (Build.VERSION.SDK_INT < 26 || getPackageManager().canRequestPackageInstalls()) {
                if (pendingVersionUrl != null && pendingVersionFilename != null) {
                    enqueueVersionDownload(pendingVersionUrl, pendingVersionFilename);
                } else {
                    checkPendingVersionDownload();
                }
            } else {
                Toast.makeText(this, "Allow this app to install updates, then try again.",
                        Toast.LENGTH_LONG).show();
            }
            pendingVersionUrl = null;
            pendingVersionFilename = null;
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

    @Override protected void onResume() {
        super.onResume();
        checkPendingVersionDownload();
    }

    @Override protected void onDestroy() {
        if (versionDownloadReceiver != null) {
            try { unregisterReceiver(versionDownloadReceiver); } catch (Exception ignored) { }
            versionDownloadReceiver = null;
        }
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
