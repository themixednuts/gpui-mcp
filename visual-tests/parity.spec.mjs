import { expect, test } from '@playwright/test';
import { execFileSync, spawn } from 'node:child_process';
import { once } from 'node:events';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

import pixelmatch from 'pixelmatch';
import { PNG } from 'pngjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const workspace = path.resolve(here, '..');
const executableName = process.platform === 'win32'
  ? 'gpui-mcp-html-visual.exe'
  : 'gpui-mcp-html-visual';
const cases = [
  { name: 'reference layout', fixture: 'parity', file: 'parity.html', state: 'baseline' },
  {
    name: 'complex nested grid and flex layout',
    fixture: 'complex-layout',
    file: 'complex-layout.html',
    state: 'baseline',
  },
  {
    name: 'behavior baseline',
    fixture: 'behaviors',
    file: 'behaviors.html',
    state: 'baseline',
    sample: { x: 100, y: 200, rgb: [24, 34, 49] },
  },
  {
    name: 'hover state',
    fixture: 'behaviors',
    file: 'behaviors.html',
    state: 'hover',
    sample: { x: 100, y: 200, rgb: [34, 49, 74] },
  },
  {
    name: 'focus state',
    fixture: 'behaviors',
    file: 'behaviors.html',
    state: 'focus',
    sample: { x: 500, y: 200, rgb: [28, 48, 64] },
  },
  {
    name: 'open disclosure dropdown',
    fixture: 'behaviors',
    file: 'behaviors.html',
    state: 'dropdown-open',
    sample: { x: 100, y: 350, rgb: [29, 41, 59] },
  },
];

function fixtureExecutable() {
  const metadata = JSON.parse(execFileSync(
    'cargo',
    ['metadata', '--no-deps', '--format-version', '1'],
    { cwd: workspace, encoding: 'utf8' },
  ));
  return path.join(metadata.target_directory, 'debug', executableName);
}

async function waitForReady(child) {
  let output = '';
  child.stdout.setEncoding('utf8');
  for await (const chunk of child.stdout) {
    output += chunk;
    if (output.split(/\r?\n/u).some((line) => line.startsWith('READY '))) {
      return;
    }
  }
  throw new Error(`GPUI fixture exited before readiness: ${output}`);
}

async function stopFixture(child) {
  if (child.exitCode !== null || child.signalCode !== null) {
    return;
  }
  child.kill();
  await Promise.race([
    once(child, 'exit'),
    new Promise((resolve) => setTimeout(resolve, 5_000)),
  ]);
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGKILL');
  }
}

async function applyBrowserState(page, testCase) {
  if (testCase.fixture === 'complex-layout') {
    const layout = await page.evaluate(() => {
      const metrics = [...document.querySelector('#metrics').children]
        .map((element) => element.getBoundingClientRect());
      const nested = [...document.querySelector('#nested-grid').children]
        .map((element) => element.getBoundingClientRect());
      return {
        display: getComputedStyle(document.querySelector('#metrics')).display,
        metricColumns: new Set(metrics.map((bounds) => bounds.x)).size,
        nestedColumns: new Set(nested.map((bounds) => bounds.x)).size,
        nestedRows: new Set(nested.map((bounds) => bounds.y)).size,
      };
    });
    expect(layout).toEqual({
      display: 'grid',
      metricColumns: 3,
      nestedColumns: 2,
      nestedRows: 2,
    });
  }

  switch (testCase.state) {
    case 'baseline':
      if (testCase.fixture === 'behaviors') {
        await expect(page.locator('#dropdown')).not.toHaveAttribute('open', '');
      }
      break;
    case 'hover':
      await page.locator('#hover-card').hover();
      expect(await page.locator('#hover-card').evaluate((element) => element.matches(':hover')))
        .toBe(true);
      break;
    case 'focus':
      await page.locator('#focus-card').focus();
      await expect(page.locator('#focus-card')).toBeFocused();
      break;
    case 'dropdown-open':
      await page.locator('#dropdown > summary').click();
      await expect(page.locator('#dropdown')).toHaveAttribute('open', '');
      await expect(page.locator('#menu')).toBeVisible();
      break;
    default:
      throw new Error(`unknown fixture state: ${testCase.state}`);
  }
}

function compareImages(chromium, gpui, diff, testCase) {
  const changedPixels = pixelmatch(
    chromium.data,
    gpui.data,
    diff.data,
    chromium.width,
    chromium.height,
    {
      threshold: 0.1,
      includeAA: false,
      diffColor: [255, 48, 96],
      aaColor: [255, 196, 0],
    },
  );
  const pixelCount = chromium.width * chromium.height;
  let absoluteChannelError = 0;
  let exactChangedPixels = 0;
  for (let offset = 0; offset < chromium.data.length; offset += 4) {
    let pixelChanged = false;
    for (let channel = 0; channel < 3; channel += 1) {
      const difference = Math.abs(chromium.data[offset + channel] - gpui.data[offset + channel]);
      absoluteChannelError += difference;
      pixelChanged ||= difference !== 0;
    }
    exactChangedPixels += Number(pixelChanged);
  }
  return {
    fixture: testCase.fixture,
    state: testCase.state,
    width: chromium.width,
    height: chromium.height,
    pixelCount,
    changedPixels,
    changedRatio: changedPixels / pixelCount,
    exactChangedPixels,
    exactChangedRatio: exactChangedPixels / pixelCount,
    normalizedMeanAbsoluteError: absoluteChannelError / (pixelCount * 3 * 255),
    pixelmatchThreshold: 0.1,
    maximumChangedRatio: 0.02,
    maximumNormalizedMeanAbsoluteError: 0.01,
  };
}

function sampleRgb(image, sample, scaleFactor) {
  const x = Math.floor(sample.x * scaleFactor);
  const y = Math.floor(sample.y * scaleFactor);
  const offset = (y * image.width + x) * 4;
  return [...image.data.slice(offset, offset + 3)];
}

for (const testCase of cases) {
  test(`Chromium and GPUI match for ${testCase.name}`, async ({ browser }, testInfo) => {
    const executable = fixtureExecutable();
    const chromiumPath = testInfo.outputPath('chromium.png');
    const gpuiPath = testInfo.outputPath('gpui.png');
    const gpuiNativePath = testInfo.outputPath('gpui-native.png');
    const diffPath = testInfo.outputPath('diff.png');
    const metricsPath = testInfo.outputPath('metrics.json');

    const fixture = spawn(
      executable,
      ['fixture', '--fixture', testCase.fixture, '--state', testCase.state],
      { cwd: workspace, stdio: ['ignore', 'pipe', 'pipe'] },
    );
    let stderr = '';
    fixture.stderr.setEncoding('utf8');
    fixture.stderr.on('data', (chunk) => {
      stderr += chunk;
    });

    try {
      await waitForReady(fixture);
      execFileSync(
        executable,
        [
          'capture',
          '--pid',
          String(fixture.pid),
          '--output',
          gpuiPath,
          '--raw-output',
          gpuiNativePath,
        ],
        { cwd: workspace, stdio: 'inherit' },
      );
    } finally {
      await stopFixture(fixture);
    }
    expect(stderr, 'GPUI fixture diagnostics').toBe('');

    const gpui = PNG.sync.read(await readFile(gpuiPath));
    const scaleFactor = gpui.width / 640;
    expect(scaleFactor).toBeGreaterThanOrEqual(0.75);
    expect(scaleFactor).toBeLessThanOrEqual(4);
    expect(gpui.height / 480).toBeCloseTo(scaleFactor, 2);

    const page = await browser.newPage({
      viewport: { width: 640, height: 480 },
      deviceScaleFactor: scaleFactor,
      colorScheme: 'dark',
      locale: 'en-US',
      timezoneId: 'UTC',
    });
    try {
      const html = await readFile(path.join(here, 'fixtures', testCase.file), 'utf8');
      await page.setContent(html, { waitUntil: 'load' });
      await page.evaluate(() => document.fonts.ready);
      await applyBrowserState(page, testCase);
      await page.screenshot({
        path: chromiumPath,
        animations: 'disabled',
        caret: 'hide',
        scale: 'device',
      });
    } finally {
      await page.close();
    }

    const chromium = PNG.sync.read(await readFile(chromiumPath));
    expect({ width: gpui.width, height: gpui.height }).toEqual({
      width: chromium.width,
      height: chromium.height,
    });
    const diff = new PNG({ width: chromium.width, height: chromium.height });
    if (testCase.sample) {
      expect(sampleRgb(chromium, testCase.sample, scaleFactor)).toEqual(testCase.sample.rgb);
      expect(sampleRgb(gpui, testCase.sample, scaleFactor)).toEqual(testCase.sample.rgb);
    }
    const metrics = {
      ...compareImages(chromium, gpui, diff, testCase),
      deviceScaleFactor: scaleFactor,
    };
    await writeFile(diffPath, PNG.sync.write(diff));
    await writeFile(metricsPath, `${JSON.stringify(metrics, null, 2)}\n`);
    await testInfo.attach('chromium', { path: chromiumPath, contentType: 'image/png' });
    await testInfo.attach('gpui', { path: gpuiPath, contentType: 'image/png' });
    await testInfo.attach('diff', { path: diffPath, contentType: 'image/png' });
    await testInfo.attach('metrics', { path: metricsPath, contentType: 'application/json' });

    expect(metrics.changedRatio, JSON.stringify(metrics)).toBeLessThanOrEqual(
      metrics.maximumChangedRatio,
    );
    expect(metrics.normalizedMeanAbsoluteError, JSON.stringify(metrics)).toBeLessThanOrEqual(
      metrics.maximumNormalizedMeanAbsoluteError,
    );
  });
}
