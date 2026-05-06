// BF GUI renderer: subscribes to a Tauri v2 Channel that pushes dirty-rect
// RGBA frames produced by the Rust interpreter thread. Each message is a
// raw ArrayBuffer laid out as:
//   u64 seq | u16 x0 | u16 y0 | u16 w | u16 h | (w*h*4) RGBA bytes
// Width/height are already clamped to the 256x256 screen on the Rust side.

const HEADER_LEN = 16;
const SCREEN_SIZE = 256;

const SPECIAL_KEYS = {
  ArrowUp: 128, ArrowDown: 129, ArrowLeft: 130, ArrowRight: 131,
  F1: 132, F2: 133, F3: 134, F4: 135,
  F5: 136, F6: 137, F7: 138, F8: 139,
};

function start() {
  const core = window.__TAURI__.core;
  const invoke = core.invoke;
  const canvas = document.getElementById('screen');
  const ctx = canvas.getContext('2d');
  const display = document.getElementById('display');
  const displayCtx = display.getContext('2d');

  const phosphor = document.createElement('canvas');
  phosphor.width = SCREEN_SIZE;
  phosphor.height = SCREEN_SIZE;
  const phosphorCtx = phosphor.getContext('2d');

  const scanlines = document.createElement('canvas');
  scanlines.width = SCREEN_SIZE;
  scanlines.height = SCREEN_SIZE;
  const scanCtx = scanlines.getContext('2d');
  const scanImg = scanCtx.createImageData(SCREEN_SIZE, SCREEN_SIZE);
  for (let y = 0; y < SCREEN_SIZE; y++) {
    for (let x = 0; x < SCREEN_SIZE; x++) {
      const i = (y * SCREEN_SIZE + x) * 4;
      const line = y % 3 === 0 ? 26 : y % 3 === 1 ? 10 : 0;
      const aperture = x % 3 === 0 ? 9 : 0;
      scanImg.data[i] = 0;
      scanImg.data[i + 1] = 0;
      scanImg.data[i + 2] = 0;
      scanImg.data[i + 3] = line + aperture;
    }
  }
  scanCtx.putImageData(scanImg, 0, 0);

  function renderCrt() {
    phosphorCtx.globalCompositeOperation = 'source-over';
    phosphorCtx.fillStyle = 'rgba(0, 0, 0, 0.18)';
    phosphorCtx.fillRect(0, 0, SCREEN_SIZE, SCREEN_SIZE);
    phosphorCtx.globalCompositeOperation = 'screen';
    phosphorCtx.filter = 'blur(0.7px) saturate(1.25)';
    phosphorCtx.drawImage(canvas, 0, 0);
    phosphorCtx.filter = 'none';
    phosphorCtx.globalCompositeOperation = 'source-over';

    displayCtx.clearRect(0, 0, SCREEN_SIZE, SCREEN_SIZE);
    displayCtx.imageSmoothingEnabled = false;
    displayCtx.drawImage(phosphor, 0, 0);

    displayCtx.globalCompositeOperation = 'screen';
    displayCtx.globalAlpha = 0.58;
    displayCtx.filter = 'blur(2px)';
    displayCtx.drawImage(canvas, 0, 0);
    displayCtx.filter = 'none';

    displayCtx.globalCompositeOperation = 'multiply';
    displayCtx.globalAlpha = 0.5;
    displayCtx.drawImage(scanlines, 0, 0);

    displayCtx.globalCompositeOperation = 'source-over';
    displayCtx.globalAlpha = 0.12;
    displayCtx.fillStyle = '#9fffe7';
    const drift = (performance.now() * 0.018) % SCREEN_SIZE;
    displayCtx.fillRect(0, drift | 0, SCREEN_SIZE, 1);
    displayCtx.globalAlpha = 1;

    requestAnimationFrame(renderCrt);
  }

  const channel = new core.Channel();
  channel.onmessage = (msg) => {
    // Tauri delivers raw payloads as ArrayBuffer (small path) or as an
    // ArrayBuffer-backed response (large path via fetch). Normalize both.
    let buf;
    if (msg instanceof ArrayBuffer) {
      buf = msg;
    } else if (ArrayBuffer.isView(msg)) {
      buf = msg.buffer.slice(msg.byteOffset, msg.byteOffset + msg.byteLength);
    } else if (msg && typeof msg === 'object' && msg.byteLength !== undefined) {
      // e.g. Blob-like or Response-like; skip until we hit a supported shape.
      return;
    } else {
      return;
    }
    if (buf.byteLength < HEADER_LEN) return;

    const view = new DataView(buf);
    const x0 = view.getUint16(8, true);
    const y0 = view.getUint16(10, true);
    const w = view.getUint16(12, true);
    const h = view.getUint16(14, true);
    const expected = HEADER_LEN + w * h * 4;
    if (buf.byteLength < expected) return;

    const pixels = new Uint8ClampedArray(buf, HEADER_LEN, w * h * 4);
    const img = new ImageData(pixels, w, h);
    ctx.putImageData(img, x0, y0);
  };

  invoke('subscribe_frames', { channel });
  renderCrt();

  // Tick pulse: inject a reserved byte (0) into the key queue ~30 Hz so BFS
  // programs can advance world state without requiring player input. Real
  // keypresses never produce 0 (see the `code > 0` gate below), so 0 is a
  // safe sentinel for "world tick, no input".
  setInterval(() => invoke('send_key', { key: 0 }), 33);

  document.addEventListener('keydown', (e) => {
    let code = 0;
    if (e.key.length === 1) {
      const c = e.key.charCodeAt(0);
      if (c < 256) code = c;
    } else {
      code = SPECIAL_KEYS[e.key] ?? 0;
    }
    if (e.repeat && (code === 32 || code === 119)) {
      e.preventDefault();
      return;
    }
    if (code > 0) {
      invoke('send_key', { key: code });
      e.preventDefault();
    }
  });
}

window.addEventListener('DOMContentLoaded', start);
