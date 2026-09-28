#include "tjsCommHead.h"
#include "Platform.h"
#include "PlatformFile.h"
#include "PlatformVideo.h"
#include "UtilStreams.h"
#include "WindowManager.h"
#include "tjsNativeMenuItem.h"

#include <android/asset_manager.h>
#include <android/asset_manager_jni.h>
#include <jni.h>

#include <condition_variable>
#include <mutex>
#include <string>
#include <vector>

namespace {
JavaVM* g_vm = nullptr;
AAssetManager* g_assets = nullptr;
jobject g_assets_ref = nullptr;
jclass g_call_class = nullptr;
jclass g_menu_data_class = nullptr;
jclass g_menu_type_class = nullptr;
std::mutex g_menu_mutex;
std::condition_variable g_menu_changed;
bool g_menu_done = false;
int g_menu_selected = -1;

struct ThreadAttachment {
    bool attached = false;
    ~ThreadAttachment() {
        if (attached && g_vm)
            g_vm->DetachCurrentThread();
    }
};
thread_local ThreadAttachment g_thread_attachment;

JNIEnv* GetEnv() {
    if (!g_vm) return nullptr;
    JNIEnv* env = nullptr;
    const jint status = g_vm->GetEnv(reinterpret_cast<void**>(&env), JNI_VERSION_1_6);
    if (status == JNI_OK) return env;
    if (status != JNI_EDETACHED ||
        g_vm->AttachCurrentThread(&env, nullptr) != JNI_OK)
        return nullptr;
    g_thread_attachment.attached = true;
    return env;
}

jstring NewString(JNIEnv* env, const ttstr& text) {
    return env->NewStringUTF(text.AsStdString().c_str());
}

void MenuFinished(int selected) {
    {
        std::lock_guard<std::mutex> lock(g_menu_mutex);
        g_menu_selected = selected;
        g_menu_done = true;
    }
    g_menu_changed.notify_one();
}
}

extern "C" JNIEXPORT void JNICALL
Java_moe_alphaly_art3m1s_KrkrNativeBridge_nativeSetAssetManager(
    JNIEnv* env, jobject, jobject assets)
{
    env->GetJavaVM(&g_vm);
    if (g_assets_ref) env->DeleteGlobalRef(g_assets_ref);
    g_assets_ref = env->NewGlobalRef(assets);
    g_assets = AAssetManager_fromJava(env, assets);
    auto save_class = [env](const char* name) -> jclass {
        jclass local = env->FindClass(name);
        if (!local) {
            env->ExceptionClear();
            return nullptr;
        }
        jclass global = static_cast<jclass>(env->NewGlobalRef(local));
        env->DeleteLocalRef(local);
        return global;
    };
    if (!g_call_class)
        g_call_class = save_class("org/tvp/krkrsdl3/KRKRCall");
    if (!g_menu_data_class)
        g_menu_data_class = save_class("org/tvp/krkrsdl3/KRKRCall$MenuItemData");
    if (!g_menu_type_class)
        g_menu_type_class = save_class("org/tvp/krkrsdl3/KRKRCall$MenuItemType");
}

tTVPMemoryStream* GetResourceStream(const ttstr& filename)
{
    if (!g_assets) return nullptr;
    AAsset* asset = AAssetManager_open(
        g_assets, filename.AsStdString().c_str(), AASSET_MODE_BUFFER);
    if (!asset) return nullptr;
    const size_t size = AAsset_getLength(asset);
    auto* stream = new tTVPMemoryStream(nullptr, size);
    size_t read = 0;
    while (read < size) {
        const int chunk = AAsset_read(asset,
            static_cast<uint8_t*>(stream->GetInternalBuffer()) + read,
            size - read);
        if (chunk <= 0) {
            delete stream;
            AAsset_close(asset);
            return nullptr;
        }
        read += static_cast<size_t>(chunk);
    }
    AAsset_close(asset);
    return stream;
}

std::string TVPGetPackageVersionString() { return "android-embedded"; }
ttstr TVPGetOSName() { return TJS_N("Android"); }

LayerVideoPlayer* CreateLayerVideoPlayer(TVPVideoEventCallback, void*) {
    return nullptr;
}
OverlayVideoPlayer* CreateOverlayVideoPlayer(TVPVideoEventCallback, void*) {
    return nullptr;
}

int TVPShowSimpleInputBox(ttstr& text, const ttstr& caption,
                          const ttstr& prompt, const std::vector<ttstr>& buttons)
{
    JNIEnv* env = GetEnv();
    if (!env || !g_call_class) return -1;
    const jmethodID show = env->GetStaticMethodID(g_call_class, "ShowInputBox",
        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;)V");
    const jmethodID wait = env->GetStaticMethodID(g_call_class, "WaitInputResult", "()I");
    const jmethodID result = env->GetStaticMethodID(g_call_class, "GetInputResult",
        "()Ljava/lang/String;");
    if (!show || !wait || !result) {
        env->ExceptionClear();
        return -1;
    }
    jclass string_class = env->FindClass("java/lang/String");
    jobjectArray labels = env->NewObjectArray(buttons.size(), string_class, nullptr);
    for (size_t index = 0; index < buttons.size(); ++index) {
        jstring label = NewString(env, buttons[index]);
        env->SetObjectArrayElement(labels, index, label);
        env->DeleteLocalRef(label);
    }
    jstring title = NewString(env, caption);
    jstring message = NewString(env, prompt);
    jstring value = NewString(env, text);
    env->CallStaticVoidMethod(g_call_class, show, title, message, value, labels);
    const jint code = env->CallStaticIntMethod(g_call_class, wait);
    jstring response = static_cast<jstring>(env->CallStaticObjectMethod(g_call_class, result));
    if (response) {
        const char* utf8 = env->GetStringUTFChars(response, nullptr);
        if (utf8) {
            text = utf8;
            env->ReleaseStringUTFChars(response, utf8);
        }
        env->DeleteLocalRef(response);
    }
    env->DeleteLocalRef(title);
    env->DeleteLocalRef(message);
    env->DeleteLocalRef(value);
    env->DeleteLocalRef(labels);
    env->DeleteLocalRef(string_class);
    if (env->ExceptionCheck()) {
        env->ExceptionClear();
        return -1;
    }
    return code;
}

extern "C" JNIEXPORT void JNICALL
Java_org_tvp_krkrsdl3_KRKRCall_nativeOnMenuItemClick(
    JNIEnv*, jclass, jint item_id, jstring)
{
    MenuFinished(item_id);
}

extern "C" JNIEXPORT void JNICALL
Java_org_tvp_krkrsdl3_KRKRCall_nativeOnMenuDismiss(JNIEnv*, jclass)
{
    MenuFinished(-1);
}

void TVPInvokeMenu(int x, int y, void* raw_menu)
{
    JNIEnv* env = GetEnv();
    if (!env || !g_call_class || !g_menu_data_class || !g_menu_type_class)
        return;
    tTJSNI_MenuItem* menu = static_cast<tTJSNI_MenuItem*>(raw_menu);
    if (!menu) {
        iTJSDispatch2* dispatch = TVPGetMenuDispatch(TVPGetActiveWindow());
        if (!dispatch) return;
        dispatch->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
            tTJSNC_MenuItem::ClassID, reinterpret_cast<iTJSNativeInstance**>(&menu));
    }
    if (!menu || menu->GetChildren().empty()) return;

    const jmethodID constructor = env->GetMethodID(g_menu_data_class, "<init>",
        "(ILjava/lang/String;)V");
    const jmethodID show = env->GetStaticMethodID(g_call_class, "showDynamicMenu",
        "(II[Lorg/tvp/krkrsdl3/KRKRCall$MenuItemData;)V");
    const jfieldID type_field = env->GetFieldID(g_menu_data_class, "type",
        "Lorg/tvp/krkrsdl3/KRKRCall$MenuItemType;");
    const jfieldID checked_field = env->GetFieldID(g_menu_data_class, "checked", "Z");
    if (!constructor || !show || !type_field || !checked_field) {
        env->ExceptionClear();
        return;
    }
    const auto& children = menu->GetChildren();
    jobjectArray entries = env->NewObjectArray(children.size(), g_menu_data_class, nullptr);
    for (size_t index = 0; index < children.size(); ++index) {
        auto* child = static_cast<tTJSNI_MenuItem*>(children[index]);
        ttstr caption;
        child->GetCaption(caption);
        if (caption.IsEmpty() || caption == TJS_N("+")) continue;
        jstring label = NewString(env, caption);
        jobject entry = env->NewObject(g_menu_data_class, constructor,
                                      static_cast<jint>(index), label);
        const char* kind = caption == TJS_N("-") ? "SEPARATOR"
            : !child->GetChildren().empty() ? "SUBMENU"
            : child->GetChecked() ? "CHECKBOX" : "NORMAL";
        const jfieldID enum_field = env->GetStaticFieldID(g_menu_type_class,
            kind, "Lorg/tvp/krkrsdl3/KRKRCall$MenuItemType;");
        if (enum_field) {
            jobject enum_value = env->GetStaticObjectField(g_menu_type_class, enum_field);
            env->SetObjectField(entry, type_field, enum_value);
            env->DeleteLocalRef(enum_value);
        }
        env->SetBooleanField(entry, checked_field, child->GetChecked());
        env->SetObjectArrayElement(entries, index, entry);
        env->DeleteLocalRef(entry);
        env->DeleteLocalRef(label);
    }
    {
        std::lock_guard<std::mutex> lock(g_menu_mutex);
        g_menu_done = false;
        g_menu_selected = -1;
    }
    env->CallStaticVoidMethod(g_call_class, show, x, y, entries);
    env->DeleteLocalRef(entries);
    if (env->ExceptionCheck()) {
        env->ExceptionClear();
        return;
    }
    int selected;
    {
        std::unique_lock<std::mutex> lock(g_menu_mutex);
        g_menu_changed.wait(lock, [] { return g_menu_done; });
        selected = g_menu_selected;
    }
    if (selected < 0 || static_cast<size_t>(selected) >= children.size()) return;
    auto* item = static_cast<tTJSNI_MenuItem*>(children[selected]);
    if (!item->GetChildren().empty())
        TVPInvokeMenu(x, y, item);
    else
        item->OnClick();
}
