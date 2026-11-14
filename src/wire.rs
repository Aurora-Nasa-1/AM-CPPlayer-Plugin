//! Positional binary codec matching the CPPlayer KMP SDK wire format.
//!
//! Little-endian, no padding, no field names, no version prefix:
//! `i32` / `i64` / one-byte bool, `i32`-prefixed UTF-8 strings and byte
//! arrays, one-byte option presence, and `i32`-prefixed lists.

use std::fmt;

pub const MAX_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_ITEMS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    TrailingData,
    InvalidBoolean,
    InvalidUtf8,
    ByteLimitExceeded,
    ItemLimitExceeded,
    InvalidLength,
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            WireError::Truncated => "Truncated wire value",
            WireError::TrailingData => "Trailing wire data",
            WireError::InvalidBoolean => "Invalid wire boolean",
            WireError::InvalidUtf8 => "Invalid UTF-8 in wire string",
            WireError::ByteLimitExceeded => "Wire byte limit exceeded",
            WireError::ItemLimitExceeded => "Wire item limit exceeded",
            WireError::InvalidLength => "Invalid wire length",
        };
        f.write_str(text)
    }
}

impl std::error::Error for WireError {}

/// Encoder that records the first failure instead of producing a partial frame.
pub struct WireWriter {
    out: Vec<u8>,
    error: Option<WireError>,
}

impl Default for WireWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl WireWriter {
    pub fn new() -> Self {
        Self {
            out: Vec::with_capacity(256),
            error: None,
        }
    }

    fn reserve(&mut self, count: usize) -> bool {
        if self.error.is_some() {
            return false;
        }
        if self.out.len().saturating_add(count) > MAX_BYTES {
            self.error = Some(WireError::ByteLimitExceeded);
            return false;
        }
        true
    }

    pub fn int(&mut self, value: i32) {
        if self.reserve(4) {
            self.out.extend_from_slice(&value.to_le_bytes());
        }
    }

    pub fn long(&mut self, value: i64) {
        if self.reserve(8) {
            self.out.extend_from_slice(&value.to_le_bytes());
        }
    }

    pub fn bool(&mut self, value: bool) {
        if self.reserve(1) {
            self.out.push(u8::from(value));
        }
    }

    pub fn bytes(&mut self, value: &[u8]) {
        if value.len() > MAX_BYTES {
            self.error = Some(WireError::ByteLimitExceeded);
            return;
        }
        self.int(value.len() as i32);
        if self.reserve(value.len()) {
            self.out.extend_from_slice(value);
        }
    }

    pub fn string(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }

    pub fn optional<T: ?Sized, F>(&mut self, value: Option<&T>, mut write: F)
    where
        F: FnMut(&mut Self, &T),
    {
        match value {
            Some(item) => {
                self.bool(true);
                write(self, item);
            }
            None => self.bool(false),
        }
    }

    pub fn list<T, F>(&mut self, value: &[T], mut write: F)
    where
        F: FnMut(&mut Self, &T),
    {
        let count = value.len().min(MAX_ITEMS);
        self.int(count as i32);
        for item in &value[..count] {
            write(self, item);
        }
    }

    pub fn finish(self) -> Result<Vec<u8>, WireError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.out),
        }
    }
}

/// Decoder that rejects truncation, trailing bytes and oversized lists.
pub struct WireReader<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> WireReader<'a> {
    pub fn new(input: &'a [u8]) -> Result<Self, WireError> {
        if input.len() > MAX_BYTES {
            return Err(WireError::ByteLimitExceeded);
        }
        Ok(Self { input, offset: 0 })
    }

    fn next(&mut self) -> Result<u8, WireError> {
        let byte = *self.input.get(self.offset).ok_or(WireError::Truncated)?;
        self.offset += 1;
        Ok(byte)
    }

    pub fn int(&mut self) -> Result<i32, WireError> {
        let end = self.offset.checked_add(4).ok_or(WireError::InvalidLength)?;
        let slice = self
            .input
            .get(self.offset..end)
            .ok_or(WireError::Truncated)?;
        self.offset = end;
        Ok(i32::from_le_bytes(slice.try_into().expect("4 bytes")))
    }

    pub fn long(&mut self) -> Result<i64, WireError> {
        let end = self.offset.checked_add(8).ok_or(WireError::InvalidLength)?;
        let slice = self
            .input
            .get(self.offset..end)
            .ok_or(WireError::Truncated)?;
        self.offset = end;
        Ok(i64::from_le_bytes(slice.try_into().expect("8 bytes")))
    }

    pub fn bool(&mut self) -> Result<bool, WireError> {
        match self.next()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(WireError::InvalidBoolean),
        }
    }

    pub fn bytes(&mut self) -> Result<Vec<u8>, WireError> {
        let length = self.int()?;
        if length < 0 {
            return Err(WireError::InvalidLength);
        }
        let length = length as usize;
        let end = self
            .offset
            .checked_add(length)
            .ok_or(WireError::InvalidLength)?;
        let slice = self
            .input
            .get(self.offset..end)
            .ok_or(WireError::InvalidLength)?;
        self.offset = end;
        Ok(slice.to_vec())
    }

    pub fn string(&mut self) -> Result<String, WireError> {
        String::from_utf8(self.bytes()?).map_err(|_| WireError::InvalidUtf8)
    }

    pub fn optional<T, F>(&mut self, mut read: F) -> Result<Option<T>, WireError>
    where
        F: FnMut(&mut Self) -> Result<T, WireError>,
    {
        if self.bool()? {
            read(self).map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn list<T, F>(&mut self, mut read: F) -> Result<Vec<T>, WireError>
    where
        F: FnMut(&mut Self) -> Result<T, WireError>,
    {
        let count = self.int()?;
        if count < 0 || count as usize > MAX_ITEMS {
            return Err(WireError::ItemLimitExceeded);
        }
        let mut items = Vec::with_capacity(count as usize);
        for _ in 0..count {
            items.push(read(self)?);
        }
        Ok(items)
    }

    /// Require the caller to consume the whole frame.
    pub fn finish(&self) -> Result<(), WireError> {
        if self.offset == self.input.len() {
            Ok(())
        } else {
            Err(WireError::TrailingData)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_primitives() {
        let mut writer = WireWriter::new();
        writer.int(-7);
        writer.long(9_007_199_254_740_991);
        writer.bool(true);
        writer.bool(false);
        writer.string("Apple Music 音乐");
        writer.bytes(&[1, 2, 3]);

        let frame = writer.finish().expect("encode");
        let mut reader = WireReader::new(&frame).unwrap();
        assert_eq!(reader.int().unwrap(), -7);
        assert_eq!(reader.long().unwrap(), 9_007_199_254_740_991);
        assert!(reader.bool().unwrap());
        assert!(!reader.bool().unwrap());
        assert_eq!(reader.string().unwrap(), "Apple Music 音乐");
        assert_eq!(reader.bytes().unwrap(), vec![1, 2, 3]);
        reader.finish().unwrap();
    }

    #[test]
    fn rejects_malformed_frames() {
        assert!(WireReader::new(&[]).unwrap().int().is_err());
        assert!(WireReader::new(&[2]).unwrap().bool().is_err());
        assert!(WireReader::new(&[0]).unwrap().finish().is_err());
        // Negative string length must not be read as a huge positive slice.
        assert!(WireReader::new(&[0xff, 0xff, 0xff, 0xff])
            .unwrap()
            .string()
            .is_err());
        // A list larger than the contract limit is rejected before allocation.
        assert!(WireReader::new(&[0xf4, 0x01, 0x00, 0x00])
            .unwrap()
            .list(|r| r.int())
            .is_err());
    }

    #[test]
    fn writer_reports_overflow_instead_of_truncating() {
        let mut writer = WireWriter::new();
        let oversized = vec![0u8; MAX_BYTES];
        writer.bytes(&oversized);
        assert_eq!(writer.finish(), Err(WireError::ByteLimitExceeded));
    }
}
