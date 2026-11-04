package dev.horizon.android;

import android.app.Activity;
import android.app.Dialog;
import android.content.Intent;
import android.graphics.Color;
import android.graphics.drawable.ColorDrawable;
import android.net.Uri;
import android.os.Bundle;
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

/** Native game host with the HTML launcher presented above the Rust surface. */
public final class HorizonActivity extends android.app.NativeActivity {
    private static final int REQUEST_DOCUMENT_IMPORT = 4101;
    private static final int REQUEST_WEB_FILE = 4102;

    static { System.loadLibrary("horizon_android"); }

    private native void nativeDocumentResult(int request, int result, String uri);
    private native void nativeWebCommand(int action, String payload);
    private native String nativeWebStatus();
    private native String nativeWebImage(String id);

    private WebView webView;
    private Dialog launcherDialog;
    private ValueCallback<Uri[]> webFileCallback;

    @Override protected void onCreate(Bundle savedInstanceState) {
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

        @JavascriptInterface public String image(String id) {
            if (id == null || id.length() > 64) return "";
            String result = nativeWebImage(id);
            return result == null ? "" : result;
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
