//! Cross-language conformance harness.
//!
//! Reads an SDK request frame from stdin (hex), invokes one operation and
//! writes the response frame (hex) to stdout. Used by the host-side test that
//! drives the Kotlin `NativeWireCodec`, so both sides are checked against the
//! same bytes.
//!
//!   echo 00000000 | cargo run --quiet --bin conformance -- capabilities

use std::io::{self, Read, Write};

use am_sdk_provider::session;

fn main() {
    let mut args = std::env::args().skip(1);
    let operation: i32 = match args.next().and_then(|value| value.parse().ok()) {
        Some(operation) => operation,
        None => {
            eprintln!(
                "usage: conformance <operation> [provider] [account] [credential] [settings]"
            );
            std::process::exit(2);
        }
    };
    let provider = args.next().unwrap_or_else(|| "apple-music".to_string());
    let account = args.next().unwrap_or_else(|| "default".to_string());
    let credential = args.next();
    let settings = args.next();

    let mut input = String::new();
    io::stdin().read_to_string(&mut input).expect("read stdin");
    let request = decode_hex(input.trim());

    let handle = match session::create(provider, account, credential, settings) {
        Ok(handle) => handle,
        Err(failure) => {
            eprintln!("create failed: {}", failure.message);
            std::process::exit(1);
        }
    };
    let response = session::invoke(handle, operation, &request);
    session::close(handle);

    io::stdout()
        .write_all(encode_hex(&response).as_bytes())
        .expect("write stdout");
    io::stdout().write_all(b"\n").expect("write newline");
}

fn decode_hex(text: &str) -> Vec<u8> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| byte.is_ascii_hexdigit())
        .collect();
    digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16).unwrap_or(0);
            let low = (pair[1] as char).to_digit(16).unwrap_or(0);
            ((high << 4) | low) as u8
        })
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let bytes = vec![0u8, 1, 15, 16, 255];
        assert_eq!(decode_hex(&encode_hex(&bytes)), bytes);
        assert_eq!(decode_hex("00 0a\nFF"), vec![0, 10, 255]);
    }
}
