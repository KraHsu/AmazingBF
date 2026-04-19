use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::io::{IoError, RuntimeIo};

/// Tape cell range reserved as the 256×256 RGB332 screen framebuffer.
pub(crate) const SCREEN_CELLS: usize = 256 * 256; // 65536
const SCREEN_START: isize = -(SCREEN_CELLS as isize); // -65536
const SCREEN_END: isize = -1;

/// Command byte used by the BFS `setpixel` built-in.
/// Sequence: 0xFE x y color → writes screen[y*256+x] = color.
const SETPIXEL_CMD: u8 = 0xFE;

/// Shared screen framebuffer: `screen[pixel]` where `pixel = -(ptr+1)`.
///
/// Cell −1 → pixel 0 = (0, 0), cell −65536 → pixel 65535 = (255, 255).
pub(crate) type ScreenBuf = Arc<Mutex<Vec<u8>>>;

/// Shared keypress queue fed by the Tauri frontend.
pub(crate) type KeyQueue = Arc<Mutex<VecDeque<u8>>>;

/// State machine for the BFS `setpixel` command protocol.
enum CmdState {
    Idle,
    WaitX,
    WaitXY(u8),
    WaitXYC(u8, u8),
}

/// GUI I/O: routes screen-buffer writes to the shared framebuffer and
/// reads input from a keypress queue fed by the Tauri frontend.
pub(crate) struct GuiIo {
    pub(crate) screen: ScreenBuf,
    pub(crate) keys: KeyQueue,
    cmd: CmdState,
}

impl GuiIo {
    pub(crate) fn new(screen: ScreenBuf, keys: KeyQueue) -> Self {
        Self { screen, keys, cmd: CmdState::Idle }
    }
}

impl RuntimeIo for GuiIo {
    fn put_byte(&mut self, ptr: isize, byte: u8) -> Result<(), IoError> {
        if (SCREEN_START..=SCREEN_END).contains(&ptr) {
            // ptr = -1 → pixel 0, ptr = -65536 → pixel 65535
            let pixel = (-(ptr + 1)) as usize;
            self.screen.lock().unwrap()[pixel] = byte;
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
            CmdState::WaitX => { self.cmd = CmdState::WaitXY(byte); }
            CmdState::WaitXY(x) => { self.cmd = CmdState::WaitXYC(x, byte); }
            CmdState::WaitXYC(x, y) => {
                let pixel = (y as usize) * 256 + (x as usize);
                self.screen.lock().unwrap()[pixel] = byte;
                self.cmd = CmdState::Idle;
            }
        }
        Ok(())
    }

    fn get_byte(&mut self, _ptr: isize) -> Result<u8, IoError> {
        loop {
            if let Some(k) = self.keys.lock().unwrap().pop_front() {
                return Ok(k);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
