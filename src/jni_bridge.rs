//! JNI exports for `cp.player.sdk.jni.NativeSession`.

#![allow(unsafe_code)]

use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{jbyteArray, jint, jlong};
use jni::JNIEnv;

use crate::session::{self, Handle};

unsafe fn read_string(env: &mut JNIEnv, value: &JString) -> Option<String> {
    if value.is_null() {
        return None;
    }
    env.get_string(value).ok().map(Into::into)
}

unsafe fn create(
    env: &mut JNIEnv,
    provider_id: JString,
    account_id: JString,
    credential: JString,
    settings: JString,
) -> jlong {
    session::create(
        read_string(env, &provider_id).unwrap_or_else(|| "apple-music".to_string()),
        read_string(env, &account_id).unwrap_or_else(|| "default".to_string()),
        read_string(env, &credential),
        read_string(env, &settings),
    )
    .unwrap_or(0)
}

unsafe fn invoke(
    env: &mut JNIEnv,
    handle: Handle,
    operation: jint,
    request: JByteArray,
) -> jbyteArray {
    let input = if request.is_null() {
        Vec::new()
    } else {
        env.convert_byte_array(&request).unwrap_or_default()
    };
    let response = session::invoke(handle, operation, &input);
    match env.byte_array_from_slice(&response) {
        Ok(array) => array.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "system" fn Java_cp_player_sdk_jni_NativeSession_createNative(
    mut env: JNIEnv,
    _class: JClass,
    provider_id: JString,
    account_id: JString,
    credential: JString,
    settings: JString,
) -> jlong {
    create(&mut env, provider_id, account_id, credential, settings)
}

#[no_mangle]
pub unsafe extern "system" fn Java_cp_player_sdk_jni_NativeSession_invokeNative(
    mut env: JNIEnv,
    _class: JClass,
    handle: jlong,
    operation: jint,
    request: JByteArray,
) -> jbyteArray {
    invoke(&mut env, handle, operation, request)
}

#[no_mangle]
pub unsafe extern "system" fn Java_cp_player_sdk_jni_NativeSession_cancelNative(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    session::cancel(handle);
}

#[no_mangle]
pub unsafe extern "system" fn Java_cp_player_sdk_jni_NativeSession_closeNative(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    session::close(handle);
}
