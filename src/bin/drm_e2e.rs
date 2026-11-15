//! Real-credential DRM end-to-end probe.
//!
//! Unlike the unit/contract tests, this binary talks to Apple with a real
//! `media-user-token`, mints a **real** Widevine challenge from a browser CDM,
//! exchanges it through the provider's `exchangeLicense`, feeds the license
//! back to the CDM, and reports how much PCM the CDM actually decrypted.
//!
//!   $env:AM_MEDIA_USER_TOKEN = "At..."
//!   cargo run --release --bin drm_e2e -- 1440857781 drm-standard
//!
//! Optional: `AM_STOREFRONT`, `AM_DEVELOPER_TOKEN`, `AM_HLS_JS` (local
//! hls.min.js; otherwise jsDelivr is used), `--no-play` to stop after signaling.
//! Challenge and license bytes are never printed or written to disk.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use am_sdk_provider::models::{
    DrmResolveRequest, Failure, LicenseRequest, LicenseResponse, ResolvedMedia,
};
use am_sdk_provider::provider::AppleProvider;
use am_sdk_provider::wire::{WireReader, WireWriter};
use serde_json::{json, Value};

const HLS_CDN: &str = "https://cdn.jsdelivr.net/npm/hls.js@1.6.16/dist/hls.min.js";

#[derive(Default, Clone)]
struct Progress {
    phase: String,
    frames: u64,
    duration: f64,
    seek_ok: bool,
    error: Option<String>,
    license_len: usize,
}

struct Bridge {
    progress: Arc<Mutex<Progress>>,
    provider: Arc<AppleProvider>,
    runtime: tokio::runtime::Handle,
    catalog_id: String,
    key_uri: String,
    local_hls: Option<Vec<u8>>,
    port: u16,
    manifest: String,
    token: String,
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let Some(catalog_id) = args.next() else {
        eprintln!("usage: drm_e2e <catalog-id> [quality] [--no-play]");
        std::process::exit(2);
    };
    let mut quality = "drm-standard".to_string();
    let mut play = true;
    for arg in args {
        match arg.as_str() {
            "--no-play" => play = false,
            value => quality = value.to_string(),
        }
    }

    let media_token = std::env::var("AM_MEDIA_USER_TOKEN").unwrap_or_default();
    if media_token.trim().is_empty() {
        eprintln!("AM_MEDIA_USER_TOKEN is required (Apple Music media-user-token)");
        std::process::exit(2);
    }
    let mut credential = json!({ "mediaUserToken": media_token });
    if let Ok(token) = std::env::var("AM_DEVELOPER_TOKEN") {
        credential["developerToken"] = json!(token);
    }
    if let Ok(storefront) = std::env::var("AM_STOREFRONT") {
        credential["storefront"] = json!(storefront);
    }

    let provider = AppleProvider::new(
        "apple-music".to_string(),
        "drm-e2e".to_string(),
        Some(credential.to_string()),
        None,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap_or_else(|failure| die("provider init", &failure));
    let provider = Arc::new(provider);

    let request = encode(|w| {
        DrmResolveRequest {
            id: catalog_id.clone(),
            quality: quality.clone(),
            keySystems: vec!["widevine".to_string()],
        }
        .write(w)
    });
    let resolved = provider
        .dispatch(am_sdk_provider::operations::tags::RESOLVEDRM, &request)
        .await
        .unwrap_or_else(|failure| die("resolveDrm", &failure));
    let resolved: ResolvedMedia = decode(&resolved, ResolvedMedia::read)
        .unwrap_or_else(|failure| die("resolveDrm response", &failure));
    let Some(descriptor) = resolved.drm.clone() else {
        die_text("resolveDrm", "no DRM descriptor (clear manifest?)");
    };

    println!(
        "resolved     actualQuality={} bitrate={:?}",
        resolved.resource.actualQuality, resolved.resource.bitrate
    );
    println!("keySystem    {}", descriptor.keySystem);
    println!(
        "keyUri       {}",
        descriptor.keyUri.as_deref().unwrap_or("<none>")
    );
    println!(
        "initData     {} bytes",
        descriptor.initData.as_ref().map_or(0, Vec::len)
    );

    let key_uri = match descriptor.keyUri.clone() {
        Some(uri) if !uri.is_empty() => uri,
        _ => die_text(
            "resolveDrm",
            "descriptor has no keyUri for license exchange",
        ),
    };
    if !play {
        println!("skipping CDM playback (--no-play)");
        return;
    }
    let Some(browser) = find_browser() else {
        die_text("browser", "no Edge/Chrome/WebView2 kernel found");
    };
    println!("browser      {browser}");

    // Bind first: the page needs the real port to build proxy URLs.
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| die_text("bind", &error.to_string()));
    let port = listener.local_addr().expect("local addr").port();
    let page_url = format!("http://127.0.0.1:{port}/");
    let token: String = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        std::process::id().hash(&mut hasher);
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_nanos())
            .unwrap_or_default()
            .hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    };

    let bridge = Arc::new(Bridge {
        progress: Arc::new(Mutex::new(Progress::default())),
        provider: provider.clone(),
        runtime: tokio::runtime::Handle::current(),
        catalog_id: catalog_id.clone(),
        key_uri,
        local_hls: std::env::var("AM_HLS_JS")
            .ok()
            .filter(|path| Path::new(path).is_file())
            .and_then(|path| std::fs::read(path).ok()),
        port,
        manifest: resolved.resource.url.clone(),
        token: token.clone(),
    });

    let running = Arc::new(AtomicBool::new(true));
    let server_bridge = bridge.clone();
    let server_running = running.clone();
    let server = std::thread::spawn(move || serve(listener, server_bridge, server_running));

    let page_with_token = format!("{page_url}?token={token}");
    let profile = std::env::temp_dir().join(format!("cpplayer-drm-e2e-{}", std::process::id()));
    let mut child = std::process::Command::new(&browser)
        .arg(format!("--app={page_with_token}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--window-size=320,120",
            "--window-position=-32000,-32000",
            "--no-first-run",
            "--no-default-browser-check",
            "--noerrdialogs",
            "--autoplay-policy=no-user-gesture-required",
            "--disable-background-timer-throttling",
            "--disable-backgrounding-occluded-windows",
            "--disable-renderer-backgrounding",
        ])
        .arg(&page_with_token)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap_or_else(|error| die_text("launch browser", &error.to_string()));

    let deadline = Instant::now() + Duration::from_secs(150);
    let mut last_phase = String::new();
    let mut outcome = None;
    while Instant::now() < deadline {
        let snapshot = bridge.progress.lock().expect("progress lock").clone();
        if !snapshot.phase.is_empty() && snapshot.phase != last_phase {
            println!("progress     {}", snapshot.phase);
            last_phase = snapshot.phase.clone();
        }
        if let Some(error) = snapshot.error.clone() {
            outcome = Some(Err(error));
            break;
        }
        if snapshot.frames > 0 && snapshot.duration >= 1.0 {
            outcome = Some(Ok(snapshot));
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    running.store(false, Ordering::SeqCst);
    let _ = child.kill();
    let _ = child.wait();
    let _ = server.join();

    match outcome {
        Some(Ok(snapshot)) => {
            println!("license      ok ({} bytes)", snapshot.license_len);
            println!(
                "decoded      {} PCM frames, {:.1}s of media",
                snapshot.frames, snapshot.duration
            );
            println!(
                "seek         {}",
                if snapshot.seek_ok {
                    "ok"
                } else {
                    "not exercised"
                }
            );
            println!("E2E          PASS");
        }
        Some(Err(error)) => {
            eprintln!("E2E          FAIL: {error}");
            std::process::exit(1);
        }
        None => {
            let snapshot = bridge.progress.lock().expect("progress lock").clone();
            eprintln!(
                "E2E          FAIL: no confirmed audio (frames={}, duration={:.1}s, last phase={})",
                snapshot.frames, snapshot.duration, snapshot.phase
            );
            std::process::exit(1);
        }
    }
}

fn serve(listener: TcpListener, bridge: Arc<Bridge>, running: Arc<AtomicBool>) {
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let bridge = bridge.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, bridge);
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
}

fn handle_connection(mut stream: TcpStream, bridge: Arc<Bridge>) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (method, target, body) = read_request(&mut stream)?;
    let (route, query) = match target.split_once('?') {
        Some((route, query)) => (route.to_string(), parse_query(query)),
        None => (target.clone(), HashMap::new()),
    };
    if route != "/hls.min.js"
        && query.get("token").map(String::as_str) != Some(bridge.token.as_str())
    {
        return respond(&mut stream, 401, "text/plain", b"unauthorized");
    }

    match (method.as_str(), route.as_str()) {
        ("GET", "/") => {
            let page = player_page(&bridge);
            respond(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                page.as_bytes(),
            )
        }
        ("GET", "/hls.min.js") => match bridge.local_hls.as_deref() {
            Some(bytes) => respond(&mut stream, 200, "application/javascript", bytes),
            None => respond(&mut stream, 404, "text/plain", b"using CDN"),
        },
        ("GET", "/manifest") => match query.get("url") {
            Some(url) if url.starts_with("https://") || url.starts_with("http://") => {
                let upstream = url.clone();
                let fetched = bridge.runtime.block_on(async {
                    reqwest::Client::new()
                        .get(&upstream)
                        .timeout(Duration::from_secs(30))
                        .send()
                        .await
                        .ok()?
                        .error_for_status()
                        .ok()?
                        .text()
                        .await
                        .ok()
                });
                match fetched {
                    Some(text) => {
                        let proxy_base = format!("http://127.0.0.1:{}/manifest", bridge.port);
                        let normalized = normalize(&text, &upstream, &proxy_base);
                        respond(
                            &mut stream,
                            200,
                            "application/vnd.apple.mpegurl",
                            normalized.as_bytes(),
                        )
                    }
                    None => respond(
                        &mut stream,
                        502,
                        "text/plain",
                        b"upstream playlist unavailable",
                    ),
                }
            }
            _ => respond(&mut stream, 400, "text/plain", b"missing url"),
        },
        ("POST", "/license") => {
            // hls.js posts the CDM challenge as a raw ArrayBuffer.
            if body.is_empty() {
                return respond(&mut stream, 400, "text/plain", b"invalid challenge");
            }
            let challenge = body;
            let request = encode(|w| {
                LicenseRequest {
                    id: bridge.catalog_id.clone(),
                    keySystem: "widevine".to_string(),
                    challenge,
                    licenseUrl: None,
                    headers: Vec::new(),
                    keyUri: Some(bridge.key_uri.clone()),
                }
                .write(w)
            });
            let response = bridge.runtime.block_on(async {
                bridge
                    .provider
                    .dispatch(am_sdk_provider::operations::tags::EXCHANGELICENSE, &request)
                    .await
            });
            match response.and_then(|payload| decode(&payload, LicenseResponse::read)) {
                Ok(license) => {
                    bridge.progress.lock().expect("lock").license_len = license.license.len();
                    // hls.js requests licenses with responseType="arraybuffer";
                    // returning base64 text makes CDM update() reject the bytes.
                    respond(
                        &mut stream,
                        200,
                        "application/octet-stream",
                        &license.license,
                    )
                }
                Err(failure) => {
                    bridge.progress.lock().expect("lock").error = Some(format!(
                        "license exchange failed: [{}] {}",
                        failure.category, failure.message
                    ));
                    respond(&mut stream, 502, "text/plain", failure.message.as_bytes())
                }
            }
        }
        ("POST", "/report") => {
            if let Ok(value) = serde_json::from_slice::<Value>(&body) {
                let mut progress = bridge.progress.lock().expect("lock");
                if let Some(phase) = value.get("phase").and_then(Value::as_str) {
                    progress.phase = phase.to_string();
                }
                if let Some(frames) = value.get("frames").and_then(Value::as_u64) {
                    progress.frames = progress.frames.max(frames);
                }
                if let Some(duration) = value.get("duration").and_then(Value::as_f64) {
                    progress.duration = progress.duration.max(duration);
                }
                if value.get("seek").and_then(Value::as_bool) == Some(true) {
                    progress.seek_ok = true;
                }
                if let Some(error) = value.get("error").and_then(Value::as_str) {
                    progress.error = Some(error.to_string());
                }
            }
            respond(&mut stream, 204, "text/plain", b"")
        }
        _ => respond(&mut stream, 404, "text/plain", b"not found"),
    }
}

fn player_page(bridge: &Bridge) -> String {
    let hls_source = if bridge.local_hls.is_some() {
        "/hls.min.js"
    } else {
        HLS_CDN
    };
    let manifest = json!(bridge.manifest);
    let license = json!(format!("http://127.0.0.1:{}/license", bridge.port));
    format!(
        r#"<!doctype html><meta charset="utf-8"><title>CPPlayer DRM E2E</title>
<audio id="a" crossorigin="anonymous"></audio>
<script src="{hls_source}"></script>
<script>
const MANIFEST = {manifest};
const LICENSE_URL = {license};
const audio = document.getElementById('a');
const report = (payload) => fetch('/report', {{ method: 'POST', body: JSON.stringify(payload) }}).catch(() => {{}});
let pcmFrames = 0;
let startedAt = 0;

function tick() {{
  const seconds = audio.currentTime || 0;
  report({{
    frames: pcmFrames,
    duration: seconds,
    seek: startedAt > 0 && seconds > startedAt + 0.5
  }});
}}

audio.addEventListener('timeupdate', tick);
audio.addEventListener('playing', () => {{
  report({{ phase: 'playing decrypted audio' }});
  startedAt = startedAt || audio.currentTime || 0;
  // Probe seeks a few seconds in once playback is confirmed.
  setTimeout(() => {{
    if (audio.duration > 10 && audio.currentTime < 3) audio.currentTime = 5;
  }}, 2500);
}});
audio.addEventListener('error', () => report({{ error: 'audio element error ' + (audio.error && audio.error.code) }}));

async function main() {{
  report({{ phase: 'requesting widevine access' }});
  const access = await navigator.requestMediaKeySystemAccess('com.widevine.alpha', [{{
    initDataTypes: ['cenc', 'cbcs', 'sinf', 'skd', 'mp4'],
    audioCapabilities: [{{ contentType: 'audio/mp4; codecs="mp4a.40.2"' }}]
  }}]);
  const keys = await access.createMediaKeys();
  await audio.setMediaKeys(keys);

  const hls = new Hls({{
    emeEnabled: true,
    widevineLicenseUrl: LICENSE_URL,
    drmSystems: {{ 'com.widevine.alpha': {{ licenseUrl: LICENSE_URL }} }}
  }});
  hls.on(Hls.Events.ERROR, (_, data) => {{
    if (data.fatal) report({{ error: (data.details || 'fatal') + ': ' + (data.error || '') }});
  }});
  hls.on(Hls.Events.KEY_SYSTEM_SESSION_UPDATE, () => report({{ phase: 'license accepted by cdm' }}));
  hls.attachMedia(audio);
  hls.on(Hls.Events.MEDIA_ATTACHED, () => hls.loadSource(MANIFEST));
  hls.on(Hls.Events.MANIFEST_PARSED, () => audio.play().catch((e) => report({{ error: 'play: ' + e }})));

  // Count real decrypted PCM: a silent ciphertext stream yields no samples.
  try {{
    const context = new AudioContext();
    const source = context.createMediaElementSource(audio);
    const analyser = context.createAnalyser();
    analyser.fftSize = 2048;
    source.connect(analyser);
    analyser.connect(context.destination);
    await context.resume();
    const buffer = new Float32Array(analyser.fftSize);
    setInterval(() => {{
      analyser.getFloatTimeDomainData(buffer);
      let energy = 0;
      for (let i = 0; i < buffer.length; i++) energy += Math.abs(buffer[i]);
      if (energy > 0.001) pcmFrames += buffer.length;
      tick();
    }}, 100);
  }} catch (error) {{
    report({{ error: 'audiocontext: ' + error }});
  }}
}}
main().catch((error) => report({{ error: 'main: ' + error }}));
</script>"#
    )
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 8 * 1024 * 1024 {
            break;
        }
    }
    let header_end = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .unwrap_or(buffer.len());
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.lines();
    let first: Vec<_> = lines.next().unwrap_or("").split_whitespace().collect();
    let method = first.first().copied().unwrap_or("").to_string();
    let target = first.get(1).copied().unwrap_or("").to_string();
    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end.min(buffer.len())..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length.min(body.len()));
    Ok((method, target, body))
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        502 => "Bad Gateway",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn parse_query(query: &str) -> HashMap<String, String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (percent_decode(key), percent_decode(value)))
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
                out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Apple uses `METHOD=ISO-23001-7` for Widevine enhanced HLS, which hls.js does
/// not recognize; `SAMPLE-AES-CTR` is the same scheme under the name it knows.
fn normalize(text: &str, upstream: &str, proxy_base: &str) -> String {
    let mut out = String::with_capacity(text.len() + 256);
    for line in text.lines() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() {
            out.push('\n');
        } else if line.starts_with("#EXT-X-KEY:") || line.starts_with("#EXT-X-SESSION-KEY:") {
            out.push_str(&rewrite_tag_uris(
                &line.replace("ISO-23001-7", "SAMPLE-AES-CTR"),
                upstream,
                proxy_base,
                false,
            ));
            out.push('\n');
        } else if line.starts_with('#') {
            out.push_str(&rewrite_tag_uris(line, upstream, proxy_base, true));
            out.push('\n');
        } else {
            let absolute = resolve(upstream, line);
            out.push_str(&map_playlist_or_segment(&absolute, proxy_base));
            out.push('\n');
        }
    }
    out
}

fn rewrite_tag_uris(line: &str, upstream: &str, proxy_base: &str, allow_proxy: bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("URI=\"") {
        out.push_str(&rest[..start + 5]);
        let after = &rest[start + 5..];
        let Some(end) = after.find('"') else {
            out.push_str(after);
            return out;
        };
        let absolute = resolve(upstream, &after[..end]);
        let mapped = if absolute.starts_with("data:") || absolute.starts_with("skd:") {
            absolute
        } else if allow_proxy && is_playlist(&absolute) {
            format!("{proxy_base}?url={}", percent_encode(&absolute))
        } else {
            absolute
        };
        out.push_str(&mapped);
        out.push('"');
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

fn map_playlist_or_segment(absolute: &str, proxy_base: &str) -> String {
    if is_playlist(absolute) {
        format!("{proxy_base}?url={}", percent_encode(absolute))
    } else {
        absolute.to_string()
    }
}

fn is_playlist(url: &str) -> bool {
    url.split('?').next().unwrap_or("").ends_with(".m3u8")
}

fn resolve(base: &str, value: &str) -> String {
    if value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("data:")
        || value.starts_with("skd:")
    {
        return value.to_string();
    }
    match url::Url::parse(base)
        .ok()
        .and_then(|base| base.join(value).ok())
    {
        Some(url) => url.to_string(),
        None => value.to_string(),
    }
}

fn find_browser() -> Option<String> {
    for candidate in [
        "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
        "C:/Program Files/Microsoft/Edge/Application/msedge.exe",
        "C:/Program Files/Google/Chrome/Application/chrome.exe",
        "C:/Program Files (x86)/Google/Chrome/Application/chrome.exe",
    ] {
        if Path::new(candidate).is_file() {
            return Some(candidate.to_string());
        }
    }
    for root in [
        "C:/Program Files (x86)/Microsoft/EdgeCore",
        "C:/Program Files (x86)/Microsoft/EdgeWebView/Application",
    ] {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        let mut versions: Vec<_> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .collect();
        versions.sort_by_key(|entry| entry.file_name());
        for entry in versions.into_iter().rev() {
            for exe in ["msedge.exe", "msedgewebview2.exe"] {
                let path = entry.path().join(exe);
                if path.is_file() {
                    return Some(path.to_string_lossy().into_owned());
                }
            }
        }
    }
    None
}

fn encode(build: impl FnOnce(&mut WireWriter)) -> Vec<u8> {
    let mut writer = WireWriter::new();
    build(&mut writer);
    writer.finish().unwrap_or_default()
}

fn decode<T>(
    bytes: &[u8],
    read: impl FnOnce(&mut WireReader) -> Result<T, am_sdk_provider::wire::WireError>,
) -> Result<T, Failure> {
    let mut reader = WireReader::new(bytes).map_err(invalid)?;
    let value = read(&mut reader).map_err(invalid)?;
    reader.finish().map_err(invalid)?;
    Ok(value)
}

fn invalid(error: am_sdk_provider::wire::WireError) -> Failure {
    Failure {
        category: "InvalidResponse".to_string(),
        message: error.to_string(),
    }
}

fn die(label: &str, failure: &Failure) -> ! {
    die_text(
        label,
        &format!("[{}] {}", failure.category, failure.message),
    )
}

fn die_text(label: &str, message: &str) -> ! {
    eprintln!("{label}: {message}");
    std::process::exit(1);
}
