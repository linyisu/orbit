//! Thin JNI lifecycle and picker callbacks; file access remains on a worker thread.
use super::{Attachment, Scan};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JString, JValue},
    sys::jint,
};
use std::sync::{Mutex, OnceLock};
use tokio::sync::oneshot;

#[path = "camera.rs"]
mod camera;
pub use camera::scan;
static VM: OnceLock<JavaVM> = OnceLock::new();
static ACTIVITY: Mutex<Option<GlobalRef>> = Mutex::new(None);
type PickerResult = Result<Option<GlobalRef>, String>;
static PICKER: Mutex<Option<oneshot::Sender<PickerResult>>> = Mutex::new(None);
static PERMISSION: Mutex<Option<oneshot::Sender<bool>>> = Mutex::new(None);

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_longbridge_nsh_AppActivity_orbitReady(
    env: JNIEnv,
    activity: JObject,
) {
    if let Ok(vm) = env.get_java_vm() {
        let _ = VM.set(vm);
    }
    if let Ok(activity) = env.new_global_ref(activity) {
        *ACTIVITY.lock().unwrap() = Some(activity);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_longbridge_nsh_AppActivity_orbitStopped(_: JNIEnv, _: JObject) {
    camera::stop();
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_longbridge_nsh_AppActivity_orbitResult(
    mut env: JNIEnv,
    _: JObject,
    request: jint,
    result: jint,
    data: JObject,
) {
    if request != 701 {
        return;
    }
    if let Some(reply) = PICKER.lock().unwrap().take() {
        let value = if result != -1 || data.is_null() {
            Ok(None)
        } else {
            env.call_method(data, "getData", "()Landroid/net/Uri;", &[])
                .and_then(|value| value.l())
                .and_then(|uri| env.new_global_ref(uri))
                .map(Some)
                .map_err(|_| "无法读取所选附件".to_owned())
        };
        let _ = reply.send(value);
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_longbridge_nsh_AppActivity_orbitPermission(
    _: JNIEnv,
    _: JObject,
    granted: jint,
) {
    if let Some(reply) = PERMISSION.lock().unwrap().take() {
        let _ = reply.send(granted == 0);
    }
}

fn activity() -> Result<GlobalRef, String> {
    ACTIVITY
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "手机界面尚未就绪".into())
}
fn env() -> Result<jni::AttachGuard<'static>, String> {
    VM.get()
        .ok_or("Android 运行时未就绪")?
        .attach_current_thread()
        .map_err(|e| e.to_string())
}

pub async fn camera_permission() -> Result<(), String> {
    let (send, receive) = oneshot::channel();
    {
        let mut pending = PERMISSION.lock().unwrap();
        if pending.is_some() {
            return Err("正在请求相机权限".into());
        }
        *pending = Some(send);
    }
    let launched = (|| -> Result<(), String> {
        let activity = activity()?;
        env()?
            .call_method(activity.as_obj(), "orbitCameraPermission", "()V", &[])
            .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = launched {
        PERMISSION.lock().unwrap().take();
        return Err(error);
    }
    match receive.await {
        Ok(true) => Ok(()),
        _ => Err("未授予相机权限，可在系统设置中开启后重试".into()),
    }
}

pub async fn pick_file() -> Result<Option<Attachment>, String> {
    let (send, receive) = oneshot::channel();
    {
        let mut pending = PICKER.lock().unwrap();
        if pending.is_some() {
            return Err("文件选择器已经打开".into());
        }
        *pending = Some(send);
    }
    let launched = (|| -> Result<(), String> {
        let activity = activity()?;
        let mut env = env()?;
        let action = env
            .new_string("android.intent.action.OPEN_DOCUMENT")
            .map_err(|e| e.to_string())?;
        let intent = env
            .new_object(
                "android/content/Intent",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&action)],
            )
            .map_err(|e| e.to_string())?;
        let category = env
            .new_string("android.intent.category.OPENABLE")
            .map_err(|e| e.to_string())?;
        env.call_method(
            &intent,
            "addCategory",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&category)],
        )
        .map_err(|e| e.to_string())?;
        let mime = env
            .new_string("*/*")
            .map_err(|e| e.to_string())?;
        env.call_method(
            &intent,
            "setType",
            "(Ljava/lang/String;)Landroid/content/Intent;",
            &[JValue::Object(&mime)],
        )
        .map_err(|e| e.to_string())?;
        env.call_method(
            activity.as_obj(),
            "orbitLaunch",
            "(Landroid/content/Intent;I)V",
            &[JValue::Object(&intent), JValue::Int(701)],
        )
        .map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = launched {
        PICKER.lock().unwrap().take();
        return Err(error);
    }
    let Some(uri) = receive
        .await
        .map_err(|_| "附件选择已取消")??
    else {
        return Ok(None);
    };
    tokio::task::spawn_blocking(move || read_file(uri))
        .await
        .map_err(|e| e.to_string())?
        .map(Some)
}

fn read_file(uri: GlobalRef) -> Result<Attachment, String> {
    let activity = activity()?;
    let mut env = env()?;
    let mut oversized = false;
    let result = (|| -> jni::errors::Result<Attachment> {
        let resolver = env
            .call_method(
                activity.as_obj(),
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )?
            .l()?;
        let cursor = env.call_method(&resolver, "query", "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[JValue::Object(uri.as_obj()), JValue::Object(&JObject::null()), JValue::Object(&JObject::null()), JValue::Object(&JObject::null()), JValue::Object(&JObject::null())])?.l()?;
        let mut name = "attachment".to_owned();
        if !cursor.is_null() {
            if env
                .call_method(&cursor, "moveToFirst", "()Z", &[])?
                .z()?
            {
                let column = env.new_string("_display_name")?;
                let index = env
                    .call_method(
                        &cursor,
                        "getColumnIndex",
                        "(Ljava/lang/String;)I",
                        &[JValue::Object(&column)],
                    )?
                    .i()?;
                if index >= 0 {
                    let value = JString::from(
                        env.call_method(
                            &cursor,
                            "getString",
                            "(I)Ljava/lang/String;",
                            &[JValue::Int(index)],
                        )?
                        .l()?,
                    );
                    name = env.get_string(&value)?.into();
                }
            }
            env.call_method(&cursor, "close", "()V", &[])?;
        }
        let mime = env
            .call_method(
                &resolver,
                "getType",
                "(Landroid/net/Uri;)Ljava/lang/String;",
                &[JValue::Object(uri.as_obj())],
            )?
            .l()?;
        let is_image = if mime.is_null() {
            false
        } else {
            String::from(env.get_string(&JString::from(mime))?).starts_with("image/")
        };
        let stream = env
            .call_method(
                &resolver,
                "openInputStream",
                "(Landroid/net/Uri;)Ljava/io/InputStream;",
                &[JValue::Object(uri.as_obj())],
            )?
            .l()?;
        let buffer = env.new_byte_array(64 * 1024)?;
        let mut bytes = Vec::new();
        let read_result = (|| -> jni::errors::Result<()> {
            loop {
                let count = env
                    .call_method(&stream, "read", "([B)I", &[JValue::Object(&buffer)])?
                    .i()?;
                if count < 0 {
                    break;
                }
                if bytes.len() + count as usize > 20 * 1024 * 1024 {
                    oversized = true;
                    break;
                }
                let chunk = env.convert_byte_array(&buffer)?;
                bytes.extend_from_slice(&chunk[..count as usize]);
            }
            Ok(())
        })();
        let closed = env.call_method(&stream, "close", "()V", &[]);
        read_result?;
        closed?;
        Ok(Attachment {
            name,
            bytes,
            image: is_image,
        })
    })();
    if result.is_err() {
        let _ = env.exception_clear();
    }
    if oversized {
        return Err("附件超过 20 MiB，请选择较小的文件".into());
    }
    result.map_err(|_| "无法读取附件，请重新选择本机文件".into())
}

/// The configured Android device name (or model) is display metadata, never an identifier.
pub fn device_name() -> String {
    let value = (|| -> Result<String, jni::errors::Error> {
        let Some(vm) = VM.get() else {
            return Ok("Android 手机".into());
        };
        let mut env = vm.attach_current_thread()?;
        let activity = ACTIVITY.lock().unwrap().clone();
        if let Some(activity) = activity {
            let configured = (|| -> Result<String, jni::errors::Error> {
                let resolver = env
                    .call_method(
                        activity.as_obj(),
                        "getContentResolver",
                        "()Landroid/content/ContentResolver;",
                        &[],
                    )?
                    .l()?;
                let key = env.new_string("device_name")?;
                let name = env
                    .call_static_method(
                        "android/provider/Settings$Global",
                        "getString",
                        "(Landroid/content/ContentResolver;Ljava/lang/String;)Ljava/lang/String;",
                        &[JValue::Object(&resolver), JValue::Object(&key)],
                    )?
                    .l()?;
                if name.is_null() {
                    return Ok(String::new());
                }
                Ok(env
                    .get_string(&JString::from(name))?
                    .into())
            })();
            if let Ok(name) = configured {
                if !name.trim().is_empty() {
                    return Ok(name);
                }
            } else {
                env.exception_clear()?;
            }
        }
        let model = env
            .get_static_field("android/os/Build", "MODEL", "Ljava/lang/String;")?
            .l()?;
        Ok(env
            .get_string(&JString::from(model))?
            .into())
    })();
    value.unwrap_or_else(|_| "Android 手机".into())
}
