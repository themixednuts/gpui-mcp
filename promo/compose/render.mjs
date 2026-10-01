// Render index.html frame-by-frame and pipe JPEGs into ffmpeg.
// usage: node render.mjs out.mp4 [from] [to] [step]   (from/to/step for stills)
import { chromium } from 'playwright';
import { spawn } from 'node:child_process';
import { pathToFileURL } from 'node:url';
import path from 'node:path';
import fs from 'node:fs';

const [out, fromA, toA, stepA] = process.argv.slice(2);
const browser = await chromium.launch({ executablePath: process.env.CHROMIUM_PATH || undefined, args: ['--force-color-profile=srgb', '--disable-lcd-text'] });
const page = await browser.newPage({ viewport: { width: 1920, height: 1080 }, deviceScaleFactor: 1 });
await page.goto(pathToFileURL(path.resolve('index.html')).href);
await page.evaluate(() => document.fonts.ready);
const total = await page.evaluate(() => window.TOTAL);
const from = fromA ? +fromA : 0, to = toA ? +toA : total, step = stepA ? +stepA : 1;

if (out.endsWith('/')) { // stills mode
  fs.mkdirSync(out, { recursive: true });
  for (let f = from; f < to; f += step) {
    await page.evaluate(f => window.renderAt(f), f);
    await page.screenshot({ path: `${out}/f${String(f).padStart(5, '0')}.png` });
  }
} else {
  const ff = spawn('ffmpeg', ['-loglevel', 'error', '-y', '-f', 'image2pipe', '-framerate', '30', '-c:v', 'mjpeg', '-i', '-',
    '-c:v', 'libx264', '-preset', 'slow', '-crf', '17', '-pix_fmt', 'yuv420p', '-movflags', '+faststart', out], { stdio: ['pipe', 'inherit', 'inherit'] });
  const t0 = Date.now();
  for (let f = from; f < to; f += step) {
    await page.evaluate(f => window.renderAt(f), f);
    const buf = await page.screenshot({ type: 'jpeg', quality: 94 });
    if (!ff.stdin.write(buf)) await new Promise(r => ff.stdin.once('drain', r));
    if (f % 150 === 0) console.log(`frame ${f}/${to} ${((Date.now() - t0) / 1000).toFixed(0)}s`);
  }
  ff.stdin.end();
  await new Promise(r => ff.on('close', r));
}
await browser.close();
