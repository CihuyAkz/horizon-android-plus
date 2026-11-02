#define _POSIX_C_SOURCE 200809L
#include <android/log.h>
#include <android/dlext.h>
#include <android/native_activity.h>
#include <jni.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define TAG "HorizonRuntimeLoader"
#define LOGE(...) __android_log_print(ANDROID_LOG_ERROR, TAG, __VA_ARGS__)
#define LOGI(...) __android_log_print(ANDROID_LOG_INFO, TAG, __VA_ARGS__)

static void *runtime_handle;
static char runtime_path[PATH_MAX];

static int valid_id(const char *id) {
    if (!id || strncmp(id, "native-", 7) != 0) return 0;
    size_t n = strlen(id);
    if (n < 14 || n > 96 || strstr(id, "..")) return 0;
    for (size_t i = 0; i < n; ++i) {
        const char c = id[i];
        if (!((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') ||
              (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.')) return 0;
    }
    return 1;
}

static int read_id(const char *path, char *out, size_t cap) {
    FILE *file = fopen(path, "rb");
    if (!file) return 0;
    size_t n = fread(out, 1, cap - 1, file);
    fclose(file);
    while (n && (out[n - 1] == '\n' || out[n - 1] == '\r' || out[n - 1] == ' ' || out[n - 1] == '\t')) --n;
    out[n] = '\0';
    return valid_id(out);
}

static void write_id(const char *path, const char *id) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC | O_NOFOLLOW, 0600);
    if (fd < 0) return;
    size_t length = strlen(id);
    if (write(fd, id, length) == (ssize_t)length) (void)write(fd, "\n", 1);
    (void)fsync(fd);
    close(fd);
}

static void *load_runtime(const char *base, const char *id) {
    char path[PATH_MAX];
    int n = snprintf(path, sizeof(path), "%s/horizon-runtimes/%s/libhorizon_runtime.so", base, id);
    if (n <= 0 || (size_t)n >= sizeof(path)) return NULL;
    // Android's linker namespace may reject dlopen() from filesDir by pathname. Supplying
    // the already-validated private file descriptor keeps runtime bundles outside the APK
    // nativeLibraryDir while still using the app's linker namespace for their dependencies.
    int fd = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd < 0) {
        LOGE("open(%s) failed: %s", path, strerror(errno));
        return NULL;
    }
    android_dlextinfo extinfo = {0};
    extinfo.flags = ANDROID_DLEXT_USE_LIBRARY_FD;
    extinfo.library_fd = fd;
    void *handle = android_dlopen_ext(path, RTLD_NOW | RTLD_LOCAL, &extinfo);
    int load_errno = errno;
    close(fd);
    errno = load_errno;
    if (!handle) {
        LOGE("android_dlopen_ext(%s) failed: %s", path, dlerror());
        return NULL;
    }
    runtime_handle = handle;
    (void)snprintf(runtime_path, sizeof(runtime_path), "%s", path);
    return handle;
}

__attribute__((visibility("default")))
void ANativeActivity_onCreate(ANativeActivity *activity, void *saved_state, size_t saved_state_size) {
    if (!activity || !activity->internalDataPath) {
        LOGE("NativeActivity has no internal data path");
        return;
    }
    char selected[128] = {0};
    char marker[PATH_MAX];
    int marker_len = snprintf(marker, sizeof(marker), "%s/horizon-runtime-selection.txt", activity->internalDataPath);
    if (marker_len >= 0 && (size_t)marker_len < sizeof(marker) && !read_id(marker, selected, sizeof(selected))) {
        selected[0] = '\0';
    }
    if (!selected[0]) {
        marker_len = snprintf(marker, sizeof(marker), "%s/horizon-runtime-default.txt", activity->internalDataPath);
        if (marker_len >= 0 && (size_t)marker_len < sizeof(marker) && !read_id(marker, selected, sizeof(selected))) {
            selected[0] = '\0';
        }
    }
    if (!selected[0] || !load_runtime(activity->internalDataPath, selected)) {
        LOGE("Selected Horizon runtime unavailable; trying APK default");
        char fallback[128] = {0};
        marker_len = snprintf(marker, sizeof(marker), "%s/horizon-runtime-default.txt", activity->internalDataPath);
        if (marker_len >= 0 && (size_t)marker_len < sizeof(marker) && !read_id(marker, fallback, sizeof(fallback))) {
            fallback[0] = '\0';
        }
        if (!fallback[0] || (selected[0] && strcmp(fallback, selected) == 0) ||
            !load_runtime(activity->internalDataPath, fallback)) {
            LOGE("No compatible Horizon runtime could be loaded");
            return;
        }
        marker_len = snprintf(marker, sizeof(marker), "%s/horizon-runtime-selection.txt", activity->internalDataPath);
        if (marker_len >= 0 && (size_t)marker_len < sizeof(marker)) write_id(marker, fallback);
    }
    typedef void (*activity_create_fn)(ANativeActivity *, void *, size_t);
    activity_create_fn create = (activity_create_fn)dlsym(runtime_handle, "ANativeActivity_onCreate");
    if (!create) {
        LOGE("Runtime %s does not export ANativeActivity_onCreate: %s", runtime_path, dlerror());
        return;
    }
    LOGI("Starting Android Horizon runtime: %s", runtime_path);
    create(activity, saved_state, saved_state_size);
}

JNIEXPORT void JNICALL
Java_dev_horizon_android_HorizonActivity_nativeWebCommand(JNIEnv *env, jobject thiz, jint action, jstring payload) {
    if (!runtime_handle) return;
    typedef void (*command_fn)(JNIEnv *, jobject, jint, jstring);
    command_fn call = (command_fn)dlsym(runtime_handle, "Java_dev_horizon_android_HorizonActivity_nativeWebCommand");
    if (call) call(env, thiz, action, payload);
}

JNIEXPORT jstring JNICALL
Java_dev_horizon_android_HorizonActivity_nativeWebStatus(JNIEnv *env, jobject thiz) {
    if (runtime_handle) {
        typedef jstring (*status_fn)(JNIEnv *, jobject);
        status_fn call = (status_fn)dlsym(runtime_handle, "Java_dev_horizon_android_HorizonActivity_nativeWebStatus");
        if (call) return call(env, thiz);
    }
    return (*env)->NewStringUTF(env, "{\"connected\":false,\"error\":\"Horizon runtime could not be loaded\"}");
}

JNIEXPORT void JNICALL
Java_dev_horizon_android_HorizonActivity_nativeDocumentResult(JNIEnv *env, jobject thiz, jint request, jint result, jstring uri) {
    if (!runtime_handle) return;
    typedef void (*document_fn)(JNIEnv *, jobject, jint, jint, jstring);
    document_fn call = (document_fn)dlsym(runtime_handle, "Java_dev_horizon_android_HorizonActivity_nativeDocumentResult");
    if (call) call(env, thiz, request, result, uri);
}
