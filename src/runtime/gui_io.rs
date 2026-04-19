//! GUI runtime I/O: keypress input + framebuffer output for `bf-gui`.
//!
//! Allocates a reserved range of negative tape indices (`SCREEN_*`) as a
//! 256×256 RGB332 framebuffer. `Setpixel` writes are coalesced into a single
//! dirty-bbox frame and forwarded to the Tauri webview through
//! `GuiShared.frame_sink`. Keypresses arrive via a `Mutex<VecDeque<u8>>` guarded
//! by `cv_key` so `GetByte` can block until a key is available.

use std::collections::VecDeque;
use std::sync::mpsc::{SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::io::{IoError, RuntimeIo};

/// Tape cell range reserved as the 256×256 RGB332 screen framebuffer.
pub(crate) const SCREEN_CELLS: usize = 256 * 256; // 65536
const SCREEN_START: isize = -(SCREEN_CELLS as isize); // -65536
const SCREEN_END: isize = -1;

/// Command byte used by the BFS `setpixel` built-in.
/// Sequence: 0xFE x y color → writes screen[y*256+x] = color.
const SETPIXEL_CMD: u8 = 0xFE;

/// Coalescing window: publish no more than once per this interval when dirty.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(8);
/// Publish early if the dirty bbox already covers this many pixels.
const PUBLISH_PIXEL_THRESHOLD: u32 = 4096;

/// Wire header (little-endian): seq:u64 | x0:u16 | y0:u16 | w:u16 | h:u16.
const FRAME_HEADER_LEN: usize = 16;

/// Shared state between the interpreter thread and the Tauri main thread.
pub(crate) struct GuiShared {
    /// Queue of pending keycodes delivered by the webview, consumed by `GetByte`.
    pub(crate) keys: Mutex<VecDeque<u8>>,
    /// Signalled when `keys` goes from empty to non-empty.
    pub(crate) cv_key: Condvar,
    /// Frame forwarder sink. Populated once a webview subscribes.
    /// Replacing the `Sender` lets a reloaded webview install a fresh forwarder.
    pub(crate) frame_tx: Mutex<Option<SyncSender<Vec<u8>>>>,
}

impl GuiShared {
    /// Build an empty shared state wrapped in an `Arc` for cross-thread sharing.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            keys: Mutex::new(VecDeque::new()),
            cv_key: Condvar::new(),
            frame_tx: Mutex::new(None),
        })
    }
}

/// State machine for the BFS `setpixel` command protocol.
enum CmdState {
    Idle,
    WaitX,
    WaitXY(u8),
    WaitXYC(u8, u8),
}

/// Inclusive bounding box of pixels written since the last publish.
#[derive(Clone, Copy)]
struct Rect {
    x0: u8,
    y0: u8,
    x1: u8,
    y1: u8,
}

impl Rect {
    fn single(x: u8, y: u8) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x,
            y1: y,
        }
    }
    fn expand(&mut self, x: u8, y: u8) {
        if x < self.x0 {
            self.x0 = x;
        }
        if x > self.x1 {
            self.x1 = x;
        }
        if y < self.y0 {
            self.y0 = y;
        }
        if y > self.y1 {
            self.y1 = y;
        }
    }
    fn pixel_count(&self) -> u32 {
        (self.x1 as u32 - self.x0 as u32 + 1) * (self.y1 as u32 - self.y0 as u32 + 1)
    }
}

/// GUI I/O: routes screen-buffer writes to a local framebuffer and publishes
/// dirty-rectangle RGBA frames through an mpsc channel to the Tauri forwarder.
pub(crate) struct GuiIo {
    shared: Arc<GuiShared>,
    /// RGB332 framebuffer, interpreter-thread-private (no locking).
    local: Box<[u8; SCREEN_CELLS]>,
    /// RGB332 → little-endian RGBA32 lookup table (matches JS palette byte-for-byte).
    lut: [u32; 256],
    dirty: Option<Rect>,
    first_dirty_at: Option<Instant>,
    cmd: CmdState,
}

impl GuiIo {
    /// Build a fresh [`GuiIo`] bound to the shared key queue / frame channel.
    pub(crate) fn new(shared: Arc<GuiShared>) -> Self {
        let mut lut = [0u32; 256];
        for b in 0..=255u32 {
            let r = (b >> 5) * 36;
            let g = ((b >> 2) & 7) * 36;
            let bl = (b & 3) * 85;
            // Little-endian byte order is [R, G, B, A] in memory, which matches
            // the layout ImageData.data expects.
            lut[b as usize] = 0xFF00_0000 | (bl << 16) | (g << 8) | r;
        }
        Self {
            shared,
            local: Box::new([0u8; SCREEN_CELLS]),
            lut,
            dirty: None,
            first_dirty_at: None,
            cmd: CmdState::Idle,
        }
    }

    fn mark_dirty(&mut self, x: u8, y: u8) {
        match self.dirty {
            None => {
                self.dirty = Some(Rect::single(x, y));
                self.first_dirty_at = Some(Instant::now());
            }
            Some(ref mut r) => r.expand(x, y),
        }
    }

    fn write_pixel(&mut self, pixel: usize, byte: u8) {
        self.local[pixel] = byte;
        let x = (pixel & 0xFF) as u8;
        let y = (pixel >> 8) as u8;
        self.mark_dirty(x, y);
        self.maybe_publish();
    }

    fn maybe_publish(&mut self) {
        let Some(rect) = self.dirty else { return };
        let over_size = rect.pixel_count() >= PUBLISH_PIXEL_THRESHOLD;
        let over_time = self
            .first_dirty_at
            .map(|t| t.elapsed() >= PUBLISH_INTERVAL)
            .unwrap_or(false);
        if over_size || over_time {
            self.publish_all();
        }
    }

    /// Force-publish the current dirty bbox (if any). Called before blocking
    /// on input so the user sees the last frame, and from `maybe_publish`.
    fn publish_all(&mut self) {
        let Some(rect) = self.dirty.take() else {
            return;
        };
        self.first_dirty_at = None;

        let bytes = self.encode_frame(rect);

        // Coalescing send: bounded channel of depth 1. If the forwarder is
        // still holding a previous frame, drain it so we always deliver the
        // newest data.
        let tx_slot = self.shared.frame_tx.lock().unwrap();
        if let Some(tx) = tx_slot.as_ref() {
            match tx.try_send(bytes) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    // Receiver was dropped (e.g. webview reload between
                    // subscribe calls). Frame is lost; next subscribe will
                    // install a fresh channel.
                }
            }
        }
    }

    fn encode_frame(&self, rect: Rect) -> Vec<u8> {
        let x0 = rect.x0 as usize;
        let y0 = rect.y0 as usize;
        let w = rect.x1 as usize - x0 + 1;
        let h = rect.y1 as usize - y0 + 1;

        let mut out = Vec::with_capacity(FRAME_HEADER_LEN + 4 * w * h);
        // seq: not strictly needed with coalescing; reserved for future diagnostics.
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(rect.x0 as u16).to_le_bytes());
        out.extend_from_slice(&(rect.y0 as u16).to_le_bytes());
        out.extend_from_slice(&(w as u16).to_le_bytes());
        out.extend_from_slice(&(h as u16).to_le_bytes());

        for y in y0..y0 + h {
            let row_start = y * 256 + x0;
            let row = &self.local[row_start..row_start + w];
            for &b in row {
                out.extend_from_slice(&self.lut[b as usize].to_le_bytes());
            }
        }
        out
    }
}

impl RuntimeIo for GuiIo {
    fn put_byte(&mut self, ptr: isize, byte: u8) -> Result<(), IoError> {
        if (SCREEN_START..=SCREEN_END).contains(&ptr) {
            // ptr = -1 → pixel 0, ptr = -65536 → pixel 65535
            let pixel = (-(ptr + 1)) as usize;
            self.write_pixel(pixel, byte);
            return Ok(());
        }
        // Positive-cell write: check for BFS setpixel command protocol.
        match self.cmd {
            CmdState::Idle if byte == SETPIXEL_CMD => {
                self.cmd = CmdState::WaitX;
            }
            CmdState::Idle => {
                use std::io::Write;
                let mut out = std::io::stdout();
                out.write_all(&[byte])
                    .map_err(|e| IoError::WriteError(e.to_string()))?;
                out.flush()
                    .map_err(|e| IoError::WriteError(e.to_string()))?;
            }
            CmdState::WaitX => {
                self.cmd = CmdState::WaitXY(byte);
            }
            CmdState::WaitXY(x) => {
                self.cmd = CmdState::WaitXYC(x, byte);
            }
            CmdState::WaitXYC(x, y) => {
                let pixel = (y as usize) * 256 + (x as usize);
                self.write_pixel(pixel, byte);
                self.cmd = CmdState::Idle;
            }
        }
        Ok(())
    }

    fn get_byte(&mut self, _ptr: isize) -> Result<u8, IoError> {
        // Publish current frame before blocking so the user sees it.
        self.publish_all();
        let mut q = self.shared.keys.lock().unwrap();
        loop {
            if let Some(k) = q.pop_front() {
                return Ok(k);
            }
            q = self.shared.cv_key.wait(q).unwrap();
        }
    }
}
