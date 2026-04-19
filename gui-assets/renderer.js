// BF GUI renderer: subscribes to a Tauri v2 Channel that pushes dirty-rect
// RGBA frames produced by the Rust interpreter thread. Each message is a
// raw ArrayBuffer laid out as:
//   u64 seq | u16 x0 | u16 y0 | u16 w | u16 h | (w*h*4) RGBA bytes
// Width/height are already clamped to the 256×256 screen on the Rust side.

const HEADER_LEN = 16;

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

    document.addEventListener('keydown', (e) => {
        let code = 0;
        if (e.key.length === 1) {
            const c = e.key.charCodeAt(0);
            if (c < 256) code = c;
        } else {
            code = SPECIAL_KEYS[e.key] ?? 0;
        }
        if (code > 0) {
            invoke('send_key', { key: code });
            e.preventDefault();
        }
    });
}

window.addEventListener('DOMContentLoaded', start);
