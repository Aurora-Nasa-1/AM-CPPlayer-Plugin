//! End-to-end wire checks that do not touch the network.

use am_sdk_provider::models::{BatchRequest, Failure, IdRequest, SearchRequest};
use am_sdk_provider::operations::{capabilities, tags, SUPPORTED};
use am_sdk_provider::session;
use am_sdk_provider::wire::{WireReader, WireWriter};

/// Read a success frame and return the payload bytes after the status byte.
fn success_payload(frame: &[u8]) -> &[u8] {
    assert_eq!(frame.first(), Some(&0), "expected a success frame");
    &frame[1..]
}

fn failure_of(frame: &[u8]) -> Failure {
    assert_eq!(frame.first(), Some(&1), "expected a failure frame");
    let mut reader = WireReader::new(&frame[1..]).unwrap();
    let failure = Failure::read(&mut reader).unwrap();
    reader.finish().unwrap();
    failure
}

#[test]
fn declares_only_implemented_capabilities() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let mut writer = WireWriter::new();
    writer.bytes(&[]);
    let frame = session::invoke(handle, tags::CAPABILITIES, &writer.finish().unwrap());
    let payload = success_payload(&frame);
    let mut reader = WireReader::new(payload).unwrap();
    let declared = am_sdk_provider::models::Capabilities::read(&mut reader).unwrap();
    reader.finish().unwrap();

    let mut expected: Vec<String> = SUPPORTED.iter().map(|item| item.to_string()).collect();
    expected.sort();
    let mut actual = declared.supported.clone();
    actual.sort();
    assert_eq!(actual, expected);
    assert!(declared
        .supported
        .contains(&capabilities::RESOLVE.to_string()));
    assert!(declared
        .supported
        .contains(&capabilities::RESOLVEDRM.to_string()));
    assert!(declared
        .supported
        .contains(&capabilities::EXCHANGELICENSE.to_string()));
    session::close(handle);
}

#[test]
fn rejects_invalid_frames_instead_of_panicking() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();

    // Truncated request for `search`.
    let frame = session::invoke(handle, tags::SEARCH, &[1, 2]);
    assert_eq!(failure_of(&frame).category, "InvalidResponse");

    // Trailing bytes after a valid no-argument request.
    let frame = session::invoke(handle, tags::CAPABILITIES, &[0, 0, 0, 0, 9]);
    assert_eq!(failure_of(&frame).category, "InvalidResponse");

    // A declared-but-foreign operation is unsupported, not silent.
    let frame = session::invoke(handle, 999, &[0, 0, 0, 0]);
    assert_eq!(failure_of(&frame).category, "Unsupported");

    session::close(handle);
}

#[test]
fn empty_search_keyword_is_rejected_before_http() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let request = SearchRequest {
        keyword: "   ".to_string(),
        limit: 30,
        offset: 0,
    };
    let mut writer = WireWriter::new();
    request.write(&mut writer);
    let frame = session::invoke(handle, tags::SEARCH, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "InvalidResponse");
    session::close(handle);
}

#[test]
fn empty_id_is_not_found() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let mut writer = WireWriter::new();
    IdRequest {
        id: "  ".to_string(),
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::GET, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "NotFound");
    session::close(handle);
}

#[test]
fn library_playlists_require_a_credential() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let request = am_sdk_provider::models::PagedIdRequest {
        id: "9084388061".to_string(),
        limit: 30,
        offset: 0,
    };
    let mut writer = WireWriter::new();
    request.write(&mut writer);
    let frame = session::invoke(handle, tags::GETUSERPLAYLISTS, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "Authentication");
    session::close(handle);
}

#[test]
fn account_session_reports_logged_out_without_credential() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let mut writer = WireWriter::new();
    writer.bytes(&[]);
    let frame = session::invoke(handle, tags::GETCURRENTACCOUNT, &writer.finish().unwrap());
    let mut reader = WireReader::new(success_payload(&frame)).unwrap();
    let account = am_sdk_provider::models::AccountSession::read(&mut reader).unwrap();
    reader.finish().unwrap();
    assert!(!account.loggedIn);
    assert!(account.userId.is_none());
    session::close(handle);
}

#[test]
fn get_many_preserves_order_and_duplicates() {
    // This path fails per item instead of failing the batch; no network is
    // reached because both IDs are empty.
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let request = BatchRequest {
        ids: vec!["".to_string(), "".to_string()],
    };
    let mut writer = WireWriter::new();
    request.write(&mut writer);
    let frame = session::invoke(handle, tags::GETMANY, &writer.finish().unwrap());
    let mut reader = WireReader::new(success_payload(&frame)).unwrap();
    let batch = am_sdk_provider::models::TrackBatch::read(&mut reader).unwrap();
    reader.finish().unwrap();
    assert_eq!(batch.items.len(), 2);
    assert!(batch.items.iter().all(|item| item.track.is_none()));
    assert!(batch.items.iter().all(|item| item.failure.is_some()));
    session::close(handle);
}

#[test]
fn c_abi_round_trips_a_capability_query() {
    use std::ffi::CString;
    use std::ptr;

    let provider = CString::new("apple-music").unwrap();
    let account = CString::new("account").unwrap();
    let mut err = [0i8; 256];
    let handle = unsafe {
        am_sdk_provider::abi::cpp_create(
            provider.as_ptr(),
            account.as_ptr(),
            ptr::null(),
            ptr::null(),
            err.as_mut_ptr(),
            err.len() as i32,
        )
    };
    assert_ne!(handle, 0);
    assert_eq!(am_sdk_provider::abi::cpp_abi_version(), 1);

    let request = [0u8; 4];
    let mut out = vec![0u8; 16 * 1024];
    let mut out_len = 0i32;
    let code = unsafe {
        am_sdk_provider::abi::cpp_invoke(
            handle,
            tags::CAPABILITIES,
            request.as_ptr(),
            request.len() as i32,
            out.as_mut_ptr(),
            out.len() as i32,
            &mut out_len,
        )
    };
    assert_eq!(code, 0);
    let frame = &out[..out_len as usize];
    assert_eq!(frame[0], 0);
    assert!(out_len > 4);

    // Capacity probing: a 1-byte buffer must request growth, not truncate.
    let mut tiny = [0u8; 1];
    let mut needed = 0i32;
    let code = unsafe {
        am_sdk_provider::abi::cpp_invoke(
            handle,
            tags::CAPABILITIES,
            request.as_ptr(),
            request.len() as i32,
            tiny.as_mut_ptr(),
            tiny.len() as i32,
            &mut needed,
        )
    };
    assert_eq!(code, 1);
    assert_eq!(needed, out_len);

    unsafe { am_sdk_provider::abi::cpp_cancel(handle) };
    unsafe { am_sdk_provider::abi::cpp_close(handle) };
}

#[test]
fn no_argument_request_is_four_zero_bytes() {
    // `Bytes` for a no-argument operation is an empty blob: length 0.
    let mut writer = WireWriter::new();
    writer.bytes(&[]);
    assert_eq!(writer.finish().unwrap(), vec![0, 0, 0, 0]);
}

#[test]
fn resolve_rejects_drm_only_accounts_before_http() {
    // The legacy `resolve` operation has no DRM field, so a non-preview
    // request with previews disabled must fail without touching the network.
    let handle = session::create(
        "apple-music".into(),
        "account".into(),
        None,
        Some(r#"{"allowPreview":"false"}"#.into()),
    )
    .unwrap();
    let mut writer = WireWriter::new();
    am_sdk_provider::models::ResolveRequest {
        id: "269573303".to_string(),
        quality: "standard".to_string(),
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::RESOLVE, &writer.finish().unwrap());
    let failure = failure_of(&frame);
    assert_eq!(failure.category, "Unsupported");
    session::close(handle);
}

#[test]
fn resolve_drm_rejects_unknown_key_systems_before_http() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let mut writer = WireWriter::new();
    am_sdk_provider::models::DrmResolveRequest {
        id: "269573303".to_string(),
        quality: "standard".to_string(),
        keySystems: vec!["not-a-key-system".to_string()],
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::RESOLVEDRM, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "InvalidResponse");
    session::close(handle);
}

#[test]
fn resolve_drm_rejects_duplicate_key_systems() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let mut writer = WireWriter::new();
    am_sdk_provider::models::DrmResolveRequest {
        id: "269573303".to_string(),
        quality: "standard".to_string(),
        keySystems: vec!["widevine".to_string(), "com.widevine.alpha".to_string()],
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::RESOLVEDRM, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "InvalidResponse");
    session::close(handle);
}

#[test]
fn resolve_drm_requires_authentication_for_library_tracks() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let mut writer = WireWriter::new();
    am_sdk_provider::models::DrmResolveRequest {
        id: "library/i.123".to_string(),
        quality: "standard".to_string(),
        keySystems: vec!["widevine".to_string()],
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::RESOLVEDRM, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "Authentication");
    session::close(handle);
}

#[test]
fn exchange_license_rejects_empty_challenge_and_missing_uri() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();

    let mut writer = WireWriter::new();
    am_sdk_provider::models::LicenseRequest {
        id: "269573303".to_string(),
        keySystem: "widevine".to_string(),
        challenge: Vec::new(),
        licenseUrl: None,
        headers: Vec::new(),
        keyUri: Some("skd://itunes.example/track".to_string()),
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::EXCHANGELICENSE, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "InvalidResponse");

    let mut writer = WireWriter::new();
    am_sdk_provider::models::LicenseRequest {
        id: "269573303".to_string(),
        keySystem: "widevine".to_string(),
        challenge: vec![1, 2, 3],
        licenseUrl: None,
        headers: Vec::new(),
        keyUri: None,
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::EXCHANGELICENSE, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "InvalidResponse");
    session::close(handle);
}

#[test]
fn exchange_license_rejects_clearkey_before_http() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    let mut writer = WireWriter::new();
    am_sdk_provider::models::LicenseRequest {
        id: "269573303".to_string(),
        keySystem: "clearkey".to_string(),
        challenge: vec![1, 2, 3],
        licenseUrl: None,
        headers: Vec::new(),
        keyUri: Some("https://cdn.example/key".to_string()),
    }
    .write(&mut writer);
    let frame = session::invoke(handle, tags::EXCHANGELICENSE, &writer.finish().unwrap());
    assert_eq!(failure_of(&frame).category, "Unsupported");
    session::close(handle);
}

#[test]
fn cancel_only_affects_the_in_flight_call() {
    let handle = session::create("apple-music".into(), "account".into(), None, None).unwrap();
    session::cancel(handle);

    let mut writer = WireWriter::new();
    writer.bytes(&[]);
    let frame = session::invoke(handle, tags::CAPABILITIES, &writer.finish().unwrap());
    assert_eq!(frame.first(), Some(&0), "a cancelled session must recover");
    session::close(handle);
}
