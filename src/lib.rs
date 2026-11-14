//! Apple Music provider for CPPlayer's `kmp-sdk-v1` contract.
//!
//! A native provider module: the host talks to it through the positional SDK
//! wire format. `cpp_*` symbols serve GraalVM Native Image, and JNI symbols
//! serve desktop JVM and Android.
//!
//! Declared operations: track search/get/batch/resolve, lyrics, playlist
//! browse/read, album browse/read, artist browse/read, and account session.
//! Everything else is rejected with a typed failure, never an empty result.

pub mod abi;
pub mod apple;
pub mod drm;
mod error;
pub mod media;
pub mod models;
pub mod operations;
pub mod provider;
pub mod session;
pub mod settings;
pub mod wire;

#[cfg(feature = "jni")]
mod jni_bridge;

pub use session::{cancel, close, create, invoke};
