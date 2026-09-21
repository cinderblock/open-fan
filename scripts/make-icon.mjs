/**
 * Generate the OpenFan source icon.
 *
 * Kept as code rather than a committed binary so the icon is reviewable, tweakable and
 * reproducible. Writes a 1024×1024 RGBA PNG; `bun run icons` then feeds it to the Tauri
 * CLI, which produces every platform size in src-tauri/icons/.
 *
 * The mark is a five-blade impeller drawn in polar coordinates: blade edges follow a
 * logarithmic sweep so they curve the way a real centrifugal fan's do.
 */
import { deflateSync } from 'node:zlib';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SIZE = 1024;
const BLADES = 5;
const SWEEP = 2.1; // how hard the blades curve
const ACCENT = [0x3d, 0x9a, 0xe8];
const ACCENT_DARK = [0x24, 0x63, 0x9c];

/** Smooth 0→1 ramp, for antialiasing edges without a rasteriser. */
const smoothstep = (edge0, edge1, x) => {
  const t = Math.min(1, Math.max(0, (x - edge0) / (edge1 - edge0)));
  return t * t * (3 - 2 * t);
};

const mix = (a, b, t) => a.map((v, i) => Math.round(v + (b[i] - v) * t));

function renderPixels() {
  const px = Buffer.alloc(SIZE * SIZE * 4);
  const c = (SIZE - 1) / 2;
  const outer = SIZE * 0.47;
  const hub = SIZE * 0.1;
  // Antialiasing width, in pixels.
  const aa = SIZE * 0.004;

  for (let y = 0; y < SIZE; y++) {
    for (let x = 0; x < SIZE; x++) {
      const dx = x - c;
      const dy = y - c;
      const r = Math.hypot(dx, dy);
      const theta = Math.atan2(dy, dx);

      // Outside the disc: fully transparent.
      const disc = 1 - smoothstep(outer - aa * 2, outer, r);
      if (disc <= 0) continue;

      // A solid rim keeps the silhouette circular once the blades are cut away, which
      // is what lets the mark still read as a fan at 16 px.
      const ringInner = outer * 0.86;
      const ring = smoothstep(ringInner - aa * 2, ringInner, r);

      let alpha;
      let color;

      if (r <= hub) {
        color = ACCENT;
        alpha = disc;
      } else {
        // Blade pattern: a periodic function of angle that twists with radius.
        const norm = (r - hub) / (outer - hub);
        const phase = theta * BLADES + SWEEP * norm * BLADES;
        const wave = Math.sin(phase);

        // Blades occupy most of each period, leaving genuine transparent gaps between
        // them. Thin lines on a filled disc disappear when the icon is scaled down.
        const width = 0.62;
        const edge = 0.07;
        const inBlade =
          smoothstep(-width, -width + edge, wave) * (1 - smoothstep(width - edge, width, wave));

        const coverage = Math.max(inBlade, ring);
        color = mix(ACCENT_DARK, ACCENT, Math.max(ring, inBlade * (0.35 + 0.65 * (1 - norm))));
        alpha = disc * coverage;
      }

      const i = (y * SIZE + x) * 4;
      px[i] = color[0];
      px[i + 1] = color[1];
      px[i + 2] = color[2];
      px[i + 3] = Math.round(alpha * 255);
    }
  }
  return px;
}

/** Minimal PNG encoder: one IHDR, one IDAT, one IEND. */
function encodePng(pixels) {
  const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE);
  for (let y = 0; y < SIZE; y++) {
    raw[y * (SIZE * 4 + 1)] = 0; // filter type: none
    pixels.copy(raw, y * (SIZE * 4 + 1) + 1, y * SIZE * 4, (y + 1) * SIZE * 4);
  }

  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body) >>> 0);
    return Buffer.concat([len, body, crc]);
  };

  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(SIZE, 0);
  ihdr.writeUInt32BE(SIZE, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // colour type: RGBA
  ihdr[10] = 0; // deflate
  ihdr[11] = 0; // adaptive filtering
  ihdr[12] = 0; // no interlace

  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', deflateSync(raw, { level: 9 })),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(buf) {
  let c = 0xffffffff;
  for (const byte of buf) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

const out = join(dirname(fileURLToPath(import.meta.url)), '..', 'assets', 'icon-source.png');
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, encodePng(renderPixels()));
console.log(`wrote ${out}`);
