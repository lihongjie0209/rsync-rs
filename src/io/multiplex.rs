#![allow(dead_code)]

use std::io::{Read, Write};

use crate::protocol::constants::{MsgCode, MPLEX_BASE};

/// A bidirectional stream that supports rsync's multiplexed I/O protocol.
///
/// Before multiplexing is activated (via `start_multiplex_in` /
/// `start_multiplex_out`), raw bytes flow through unchanged.  Once activated,
/// the sender wraps every payload in a 4-byte little-endian header:
///
/// ```text
/// bits 31-24  MPLEX_BASE (7) + msg_code
/// bits 23-0   payload length (24-bit unsigned)
/// ```
///
/// `MSG_DATA` (code 0, header byte = 7) carries raw file/protocol data.
/// All other codes carry out-of-band diagnostic or control messages.
pub struct MultiplexStream {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    /// Whether incoming bytes are wrapped in multiplex headers.
    pub multiplex_in: bool,
    /// Whether outgoing bytes should be wrapped in multiplex headers.
    pub multiplex_out: bool,
    /// Bytes remaining in the current inbound MSG_DATA payload.
    pending_data: usize,
    /// The last non-data message code received (informational).
    pending_msg: Option<MsgCode>,
}

impl MultiplexStream {
    pub fn new(reader: Box<dyn Read + Send>, writer: Box<dyn Write + Send>) -> Self {
        Self {
            reader,
            writer,
            multiplex_in: false,
            multiplex_out: false,
            pending_data: 0,
            pending_msg: None,
        }
    }

    /// Signal that the remote will start sending multiplexed frames.
    pub fn start_multiplex_in(&mut self) {
        self.multiplex_in = true;
    }

    /// Signal that we will start sending multiplexed frames.
    pub fn start_multiplex_out(&mut self) {
        self.multiplex_out = true;
    }

    // ── Raw (pre-multiplex) I/O ───────────────────────────────────────────

    /// Read exactly `buf.len()` raw bytes, bypassing multiplex framing.
    pub fn read_exact(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        self.reader.read_exact(buf)?;
        Ok(())
    }

    /// Write raw bytes, bypassing multiplex framing.
    pub fn write_all(&mut self, buf: &[u8]) -> anyhow::Result<()> {
        self.writer.write_all(buf)?;
        Ok(())
    }

    // ── Multiplexed I/O ───────────────────────────────────────────────────

    /// Read exactly `buf.len()` bytes of MSG_DATA, demultiplexing as needed.
    ///
    /// Non-data frames (INFO, ERROR, WARNING, …) received in-stream are
    /// dispatched to `handle_msg` and their payloads consumed before resuming
    /// the data read.
    pub fn read_data(&mut self, buf: &mut [u8]) -> anyhow::Result<()> {
        if !self.multiplex_in {
            self.reader.read_exact(buf)?;
            return Ok(());
        }

        let mut pos = 0;
        while pos < buf.len() {
            // Refill the pending data counter if we've exhausted the current frame.
            while self.pending_data == 0 {
                let (code, len) = self.read_header()?;
                if code == MsgCode::Data {
                    self.pending_data = len;
                } else {
                    self.pending_msg = Some(code);
                    self.handle_msg(code, len)?;
                    self.pending_msg = None;
                }
            }

            let to_read = (buf.len() - pos).min(self.pending_data);
            self.reader.read_exact(&mut buf[pos..pos + to_read])?;
            self.pending_data -= to_read;
            pos += to_read;
        }
        Ok(())
    }

    /// Write `buf` as one or more MSG_DATA frames.
    ///
    /// If multiplexing is not yet active, the bytes are written raw.
    pub fn write_data(&mut self, buf: &[u8]) -> anyhow::Result<()> {
        if !self.multiplex_out {
            self.writer.write_all(buf)?;
            return Ok(());
        }
        // The 24-bit length field limits each frame to 16 MiB − 1.
        let mut offset = 0;
        while offset < buf.len() {
            let chunk = (buf.len() - offset).min(0x00FF_FFFF);
            self.write_frame(MsgCode::Data, &buf[offset..offset + chunk])?;
            offset += chunk;
        }
        Ok(())
    }

    /// Write an out-of-band multiplexed message (e.g. INFO, ERROR, REDO).
    ///
    /// The caller must ensure multiplexing is active (`start_multiplex_out`).
    pub fn write_msg(&mut self, code: MsgCode, data: &[u8]) -> anyhow::Result<()> {
        self.write_frame(code, data)
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> anyhow::Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    // ── Private helpers ───────────────────────────────────────────────────

    /// Read and decode a 4-byte multiplex frame header.
    ///
    /// Returns `(msg_code, payload_len)`.
    fn read_header(&mut self) -> anyhow::Result<(MsgCode, usize)> {
        let mut hdr = [0u8; 4];
        self.reader.read_exact(&mut hdr)?;
        let raw = u32::from_le_bytes(hdr);

        let raw_code = (raw >> 24) as u8;
        let len = (raw & 0x00FF_FFFF) as usize;

        let code_val = raw_code.checked_sub(MPLEX_BASE).ok_or_else(|| {
            anyhow::anyhow!(
                "multiplex header code 0x{:02X} is below MPLEX_BASE {}",
                raw_code,
                MPLEX_BASE
            )
        })?;

        let code = MsgCode::from_u8(code_val).ok_or_else(|| {
            anyhow::anyhow!("unknown multiplex message code: {}", code_val)
        })?;

        Ok((code, len))
    }

    /// Consume and log/dispatch a non-data frame payload.
    fn handle_msg(&mut self, code: MsgCode, len: usize) -> anyhow::Result<()> {
        let mut payload = vec![0u8; len];
        self.reader.read_exact(&mut payload)?;
        let text = String::from_utf8_lossy(&payload);
        let text = text.trim_end_matches(['\n', '\r']);

        match code {
            MsgCode::Info | MsgCode::Client => {
                log::info!("[remote] {}", text);
            }
            MsgCode::Warning => {
                log::warn!("[remote warning] {}", text);
            }
            MsgCode::Error | MsgCode::ErrorXfer | MsgCode::ErrorSocket | MsgCode::ErrorUtf8 => {
                log::error!("[remote error] {}", text);
            }
            MsgCode::Log => {
                log::debug!("[remote log] {}", text);
            }
            _ => {
                log::debug!("unhandled multiplex msg code={:?} len={}", code, len);
            }
        }
        Ok(())
    }

    /// Encode and write a single multiplexed frame.
    fn write_frame(&mut self, code: MsgCode, data: &[u8]) -> anyhow::Result<()> {
        let len = data.len();
        anyhow::ensure!(
            len <= 0x00FF_FFFF,
            "multiplex payload too large: {} bytes (max 16 MiB − 1)",
            len
        );
        // Header: bits 31-24 = MPLEX_BASE + code, bits 23-0 = length.
        let hdr = ((MPLEX_BASE as u32 + code as u32) << 24) | (len as u32);
        self.writer.write_all(&hdr.to_le_bytes())?;
        self.writer.write_all(data)?;
        Ok(())
    }
}

// ── MplexWriter ───────────────────────────────────────────────────────────────

/// A simple writer that wraps outgoing bytes in rsync MSG_DATA multiplex frames.
///
/// Before `enable()` is called, bytes pass through raw (unchanged).
/// After `enable()`, each `write()` emits a 4-byte LE frame header followed by
/// the payload:
///   `bits 31-24 = MPLEX_BASE + MsgCode::Data (= 7 + 0 = 7)`
///   `bits 23-0  = payload length`
pub struct MplexWriter<W: Write> {
    inner: W,
    enabled: bool,
}

impl<W: Write> MplexWriter<W> {
    pub fn new(inner: W) -> Self {
        MplexWriter { inner, enabled: false }
    }

    /// Activate multiplex framing on all subsequent writes.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Write an out-of-band message frame (must be called after `enable()`).
    pub fn write_msg(&mut self, code: MsgCode, data: &[u8]) -> std::io::Result<()> {
        let len = data.len();
        assert!(len <= 0x00FF_FFFF, "mplex payload too large");
        let hdr = ((MPLEX_BASE as u32 + code as u32) << 24) | (len as u32);
        self.inner.write_all(&hdr.to_le_bytes())?;
        self.inner.write_all(data)
    }
}

impl<W: Write> Write for MplexWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.enabled {
            return self.inner.write(buf);
        }
        // Limit to 24-bit length field.
        let chunk = buf.len().min(0x00FF_FFFF);
        let hdr = ((MPLEX_BASE as u32) << 24) | (chunk as u32); // code=0 (Data), header byte=7
        self.inner.write_all(&hdr.to_le_bytes())?;
        self.inner.write_all(&buf[..chunk])?;
        Ok(chunk)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ── MplexReader ───────────────────────────────────────────────────────────────

/// A reader that demultiplexes incoming rsync MSG_DATA frames.
///
/// Before `enable()` is called, bytes are read raw (unchanged).
/// After `enable()`, reads expect a 4-byte LE frame header before each payload.
/// Non-data frames (INFO, ERROR, WARNING, …) are consumed and logged; only
/// MSG_DATA bytes are returned to the caller.
pub struct MplexReader<R: Read> {
    inner: R,
    enabled: bool,
    /// Bytes remaining in the current inbound MSG_DATA frame.
    pending: usize,
}

impl<R: Read> MplexReader<R> {
    pub fn new(inner: R) -> Self {
        MplexReader { inner, enabled: false, pending: 0 }
    }

    /// Activate multiplex demultiplexing on all subsequent reads.
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Read and decode the next 4-byte frame header.
    fn read_header(&mut self) -> std::io::Result<(MsgCode, usize)> {
        let mut hdr = [0u8; 4];
        self.inner.read_exact(&mut hdr)?;
        let raw = u32::from_le_bytes(hdr);
        let raw_code = (raw >> 24) as u8;
        let len = (raw & 0x00FF_FFFF) as usize;
        let code_val = raw_code.saturating_sub(MPLEX_BASE);
        let code = MsgCode::from_u8(code_val).unwrap_or(MsgCode::Data);
        crate::rdebug!("[mux-in] frame: code={:?}({}), raw_byte=0x{:02x}, len={}", code, code_val, raw_code, len);
        Ok((code, len))
    }

    /// Consume and log a non-data frame payload.
    fn drain_msg(&mut self, code: MsgCode, len: usize) -> std::io::Result<()> {
        let mut payload = vec![0u8; len];
        self.inner.read_exact(&mut payload)?;
        let text = String::from_utf8_lossy(&payload);
        let text = text.trim_end_matches(['\n', '\r']);
        match code {
            MsgCode::Info | MsgCode::Client => log::info!("[remote] {}", text),
            MsgCode::Warning => log::warn!("[remote warning] {}", text),
            MsgCode::Error | MsgCode::ErrorXfer | MsgCode::ErrorSocket | MsgCode::ErrorUtf8 => {
                log::error!("[remote error] {}", text);
            }
            _ => log::debug!("unhandled mplex msg code={:?} len={}", code, len),
        }
        Ok(())
    }
}

impl<R: Read> Read for MplexReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if !self.enabled {
            return self.inner.read(buf);
        }

        // Refill pending counter if the current DATA frame is exhausted.
        while self.pending == 0 {
            let (code, len) = self.read_header()?;
            if code == MsgCode::Data {
                if len == 0 {
                    // Empty DATA frame — skip it.
                    continue;
                }
                self.pending = len;
            } else {
                self.drain_msg(code, len)?;
            }
        }

        let to_read = buf.len().min(self.pending);
        let n = self.inner.read(&mut buf[..to_read])?;
        self.pending -= n;
        Ok(n)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    /// A `Write + Send + 'static` wrapper around a shared Vec, usable in tests.
    #[derive(Clone)]
    struct SharedVec(Arc<Mutex<Vec<u8>>>);

    impl SharedVec {
        fn new() -> Self {
            SharedVec(Arc::new(Mutex::new(Vec::new())))
        }
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }
    }

    impl Write for SharedVec {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn make_stream(input: Vec<u8>) -> MultiplexStream {
        MultiplexStream::new(Box::new(Cursor::new(input)), Box::new(SharedVec::new()))
    }

    fn make_write_stream() -> (MultiplexStream, SharedVec) {
        let spy = SharedVec::new();
        let s = MultiplexStream::new(
            Box::new(Cursor::new(Vec::<u8>::new())),
            Box::new(spy.clone()),
        );
        (s, spy)
    }

    /// Build a valid multiplex frame header (little-endian u32).
    fn mux_header(code: MsgCode, len: usize) -> [u8; 4] {
        let raw = ((MPLEX_BASE as u32 + code as u32) << 24) | (len as u32);
        raw.to_le_bytes()
    }

    #[test]
    fn raw_read_passthrough() {
        let mut s = make_stream(vec![1, 2, 3, 4]);
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn mux_read_data_simple() {
        let mut input = mux_header(MsgCode::Data, 3).to_vec();
        input.extend_from_slice(&[0xAA, 0xBB, 0xCC]);

        let mut s = make_stream(input);
        s.start_multiplex_in();

        let mut buf = [0u8; 3];
        s.read_data(&mut buf).unwrap();
        assert_eq!(buf, [0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn mux_read_data_across_frames() {
        // Two MSG_DATA frames read as one contiguous buffer.
        let mut input = Vec::new();
        input.extend_from_slice(&mux_header(MsgCode::Data, 2));
        input.extend_from_slice(&[0x01, 0x02]);
        input.extend_from_slice(&mux_header(MsgCode::Data, 2));
        input.extend_from_slice(&[0x03, 0x04]);

        let mut s = make_stream(input);
        s.start_multiplex_in();

        let mut buf = [0u8; 4];
        s.read_data(&mut buf).unwrap();
        assert_eq!(buf, [0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn mux_read_skips_info_frame() {
        // An INFO frame interleaved before a DATA frame.
        let mut input = Vec::new();
        input.extend_from_slice(&mux_header(MsgCode::Info, 5));
        input.extend_from_slice(b"hello");
        input.extend_from_slice(&mux_header(MsgCode::Data, 2));
        input.extend_from_slice(&[0xDE, 0xAD]);

        let mut s = make_stream(input);
        s.start_multiplex_in();

        let mut buf = [0u8; 2];
        s.read_data(&mut buf).unwrap();
        assert_eq!(buf, [0xDE, 0xAD]);
    }

    #[test]
    fn mux_write_data_produces_correct_header() {
        let (mut s, spy) = make_write_stream();
        s.start_multiplex_out();
        s.write_data(&[0x11, 0x22, 0x33]).unwrap();

        let out = spy.bytes();
        // 4-byte header + 3-byte payload
        assert_eq!(out.len(), 7);
        let hdr = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        assert_eq!((hdr >> 24) as u8, MPLEX_BASE + MsgCode::Data as u8);
        assert_eq!(hdr & 0x00FF_FFFF, 3);
        assert_eq!(&out[4..], &[0x11, 0x22, 0x33]);
    }

    #[test]
    fn mux_write_msg_produces_correct_header() {
        let (mut s, spy) = make_write_stream();
        s.start_multiplex_out();
        s.write_msg(MsgCode::Info, b"test").unwrap();

        let out = spy.bytes();
        let hdr = u32::from_le_bytes([out[0], out[1], out[2], out[3]]);
        assert_eq!((hdr >> 24) as u8, MPLEX_BASE + MsgCode::Info as u8);
        assert_eq!(&out[4..], b"test");
    }

    #[test]
    fn header_encoding_matches_c_spec() {
        // C: SIVAL(header, 0, ((MPLEX_BASE + MSG_DATA) << 24) + len)
        // MPLEX_BASE=7, MSG_DATA=0, len=100 → LE bytes of (7<<24)|100
        let expected = ((7u32 << 24) | 100u32).to_le_bytes();

        let (mut s, spy) = make_write_stream();
        s.start_multiplex_out();
        s.write_data(&vec![0u8; 100]).unwrap();

        assert_eq!(&spy.bytes()[..4], &expected);
    }
}
