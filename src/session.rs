//! Session lifecycle shared by the JNI and C ABI exports.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use crate::models::Failure;
use crate::provider::AppleProvider;
use crate::wire::WireWriter;

pub type Handle = i64;

pub struct Session {
    pub provider: AppleProvider,
    cancelled: Arc<AtomicBool>,
    closed: AtomicBool,
}

impl Session {
    pub fn new(
        provider_id: String,
        account_id: String,
        credential: Option<String>,
        settings_json: Option<String>,
    ) -> Result<Self, Failure> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let provider = AppleProvider::new(
            provider_id,
            account_id,
            credential,
            settings_json,
            cancelled.clone(),
        )?;
        Ok(Self {
            provider,
            cancelled,
            closed: AtomicBool::new(false),
        })
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.provider.cancel();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .thread_name("am-provider")
            .build()
            .expect("provider runtime")
    })
}

/// Create a boxed session and return its opaque handle.
pub fn create(
    provider_id: String,
    account_id: String,
    credential: Option<String>,
    settings_json: Option<String>,
) -> Result<Handle, Failure> {
    let session = Session::new(provider_id, account_id, credential, settings_json)?;
    Ok(Box::into_raw(Box::new(session)) as Handle)
}

/// Invoke one operation and frame it as `[status] [payload]`.
pub fn invoke(handle: Handle, operation: i32, request: &[u8]) -> Vec<u8> {
    let Some(session) = session(handle) else {
        return error_frame(&Failure {
            category: "Runtime".to_string(),
            message: "Provider session handle is invalid".to_string(),
        });
    };
    if session.is_closed() {
        return error_frame(&Failure {
            category: "Runtime".to_string(),
            message: "Provider session is closed".to_string(),
        });
    }
    // Cancellation targets the invocation that is in flight. Clear the flag so
    // the next host call on the same session starts unencumbered.
    session.cancelled.store(false, Ordering::SeqCst);
    let provider = &session.provider;
    match runtime().block_on(provider.dispatch(operation, request)) {
        Ok(payload) => success_frame(payload),
        Err(failure) => error_frame(&failure),
    }
}

fn success_frame(payload: Vec<u8>) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 1);
    frame.push(0);
    frame.extend_from_slice(&payload);
    frame
}

/// Frame a failure as `[1] [category] [message]`.
pub fn error_frame(failure: &Failure) -> Vec<u8> {
    let wire = Failure {
        category: sanitize(&failure.category, 128, "Runtime"),
        message: sanitize(&failure.message, 4096, "Provider failure"),
    };
    let mut writer = WireWriter::new();
    wire.write(&mut writer);
    let payload = writer.finish().unwrap_or_else(|_| {
        let mut fallback = WireWriter::new();
        Failure {
            category: "Runtime".to_string(),
            message: "Provider failure".to_string(),
        }
        .write(&mut fallback);
        fallback.finish().unwrap_or_default()
    });
    let mut frame = Vec::with_capacity(payload.len() + 1);
    frame.push(1);
    frame.extend_from_slice(&payload);
    frame
}

fn sanitize(value: &str, limit: usize, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    trimmed.chars().take(limit).collect()
}

pub fn cancel(handle: Handle) {
    if let Some(session) = session(handle) {
        session.cancel();
    }
}

pub fn close(handle: Handle) {
    if handle == 0 {
        return;
    }
    // `Box::from_raw` runs once; a repeated close is rejected by the closed
    // flag, and the pointer is never touched again.
    let session = unsafe { Box::from_raw(handle as *mut Session) };
    session.close();
    drop(session);
}

fn session(handle: Handle) -> Option<&'static Session> {
    if handle == 0 {
        return None;
    }
    // The host serializes per-session calls and closes only after draining.
    Some(unsafe { &*(handle as *const Session) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::WireReader;

    fn read_failure(frame: &[u8]) -> (String, String) {
        assert_eq!(frame[0], 1);
        let mut reader = WireReader::new(&frame[1..]).unwrap();
        let category = reader.string().unwrap();
        let message = reader.string().unwrap();
        reader.finish().unwrap();
        (category, message)
    }

    #[test]
    fn frames_failures_with_status_byte() {
        let frame = error_frame(&Failure {
            category: "NotFound".to_string(),
            message: "missing".to_string(),
        });
        assert_eq!(
            read_failure(&frame),
            ("NotFound".to_string(), "missing".to_string())
        );
    }

    #[test]
    fn replaces_overlong_failure_fields() {
        let frame = error_frame(&Failure {
            category: "  ".to_string(),
            message: "x".repeat(10_000),
        });
        let (category, message) = read_failure(&frame);
        assert_eq!(category, "Runtime");
        assert_eq!(message.chars().count(), 4096);
    }

    #[test]
    fn invalid_handle_is_a_framed_failure() {
        let frame = invoke(0, 1, &[0, 0, 0, 0]);
        let (category, _) = read_failure(&frame);
        assert_eq!(category, "Runtime");
    }
}
