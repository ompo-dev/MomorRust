import { expect, test } from '@playwright/test';

test('API readings match the browser and internal controls preserve focus', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto('/');
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
  await page.getByLabel('Campo de teste').fill('Momor');
  await page.getByLabel('Sele\u00e7\u00e3o', { exact: true }).selectOption('b');
  await page.getByRole('button', { name: 'Entradas', exact: true }).click();
  await page.getByRole('button', { name: 'Todos', exact: true }).click();
  await expect(page.locator('tr[data-direction="saida"]')).toHaveCount(0);
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
  const observed = await page.evaluate(() => ({ focused: document.hasFocus(), visibility: document.visibilityState, hidden: document.hidden }));
  expect(observed).toEqual({ focused: true, visibility: 'visible', hidden: false });
  expect(errors).toEqual([]);
  await page.screenshot({ path: 'artifacts/desktop-green.png', fullPage: true });
});

test('page lifecycle signals become red and synthetic events are identified', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide')));
  await expect(page.locator('main')).toHaveAttribute('data-status', 'away');
  const departure = page.locator('tr[data-direction="saida"]');
  await expect(departure).toContainText('pagehide');
  await expect(departure).toContainText('Sint\u00e9tico');
  await page.screenshot({ path: 'artifacts/desktop-red.png', fullPage: true });
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow')));
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
  await expect(page.locator('tr[data-direction="entrada"]')).toHaveCount(1);
});

test('a window blur signal is detected independently of hasFocus', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => window.dispatchEvent(new FocusEvent('blur')));
  await expect(page.locator('main')).toHaveAttribute('data-status', 'away');
  await expect(page.getByText('Evento blur observado')).toBeVisible();
  await expect(page.locator('tr[data-direction="saida"]')).toContainText('blur');
  await page.evaluate(() => window.dispatchEvent(new FocusEvent('focus')));
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
});

test('real navigation retains departure evidence and existing logs', async ({ page }) => {
  await page.goto('/');
  await page.goto('/?second-document');
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
  const event = page.locator('tr[data-source="pagehide"][data-scope="pagina"]');
  await expect(event).toHaveCount(1);
  await expect(event).toContainText('Nativo');
  await expect(page.locator('tr[data-source="inicio"][data-scope="pagina"]')).toHaveCount(2);
});

test('logs export valid JSON, filters work, and clear resets them', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide')));
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow')));
  await page.getByRole('button', { name: 'Sa\u00eddas', exact: true }).click();
  await expect(page.locator('tbody tr')).toHaveCount(1);
  await page.getByRole('button', { name: 'Todos', exact: true }).click();
  const downloadReady = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Exportar logs JSON' }).click();
  const download = await downloadReady;
  const stream = await download.createReadStream();
  if (!stream) throw new Error('Missing downloaded JSON stream');
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk));
  const data = JSON.parse(Buffer.concat(chunks).toString('utf8'));
  expect(data.version).toBe(2);
  expect(data.exits).toBe(1);
  expect(data.events.some((event: { direction: string }) => event.direction === 'entrada')).toBe(true);
  await page.getByRole('button', { name: 'Limpar logs' }).click();
  await expect(page.locator('tbody')).toContainText('limpeza');
  await page.reload();
  await expect(page.locator('tbody')).toContainText('limpeza');
});

test('same and cross origin frame probes connect and focus stays inside the page', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Iframes', exact: true }).click();
  await expect(page.getByText('Conectado', { exact: true })).toHaveCount(2);
  await page.frameLocator('iframe[title="Sonda Mesma origem"]').getByLabel('Campo do iframe').fill('Campo A');
  await expect(page.locator('main').first()).toHaveAttribute('data-status', 'active');
  await page.frameLocator('iframe[title="Sonda Outra origem"]').getByLabel('Campo do iframe').fill('Campo B');
  await expect(page.locator('main').first()).toHaveAttribute('data-status', 'active');
  await expect(page.locator('tr[data-direction="saida"]')).toHaveCount(0);
  await expect(page.locator('tbody')).toContainText('cross-origin');
  await page.screenshot({ path: 'artifacts/desktop-frames.png', fullPage: true });
});

test('a main-thread stall produces hints without inventing a departure', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => {
    setTimeout(() => {
      const until = performance.now() + 350;
      while (performance.now() < until) { /* Deliberate bounded stall in the owned test fixture. */ }
    }, 0);
  });
  await expect(page.getByLabel('Indicios ativos')).toContainText('main.longtask');
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
  await expect(page.locator('tr[data-direction="saida"]')).toHaveCount(0);
  await page.getByRole('button', { name: 'Ind\u00edcios', exact: true }).click();
  await expect(page.locator('tbody')).toContainText('main.longtask');
});

test('configuration persists and worker heartbeats are visible', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Configura\u00e7\u00e3o', exact: true }).click();
  const threshold = page.getByLabel('Inatividade (s)');
  await threshold.fill('90');
  await threshold.press('Enter');
  await page.getByLabel('Ind\u00edcios de inatividade', { exact: true }).uncheck();
  await page.reload();
  await page.getByRole('button', { name: 'Configura\u00e7\u00e3o', exact: true }).click();
  await expect(page.getByLabel('Inatividade (s)')).toHaveValue('90');
  await expect(page.getByLabel('Ind\u00edcios de inatividade', { exact: true })).not.toBeChecked();
  await page.getByRole('button', { name: 'Sinais', exact: true }).click();
  await expect(page.getByLabel('Sondas de atividade')).toContainText(/Worker: [1-9]/);
});

test('another tab cannot overwrite this tab history', async ({ page, context }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Limpar logs' }).click();
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pagehide')));
  await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow')));
  const other = await context.newPage();
  await other.goto('/');
  await other.getByRole('button', { name: 'Limpar logs' }).click();
  await page.reload();
  await expect(page.locator('tr[data-source="pagehide"][data-direction="saida"]').first()).toBeVisible();
  await other.close();
});

test('forged iframe message from the parent is ignored', async ({ page }) => {
  await page.goto('/');
  await page.evaluate(() => {
    const frame = document.querySelector<HTMLIFrameElement>('iframe');
    if (!frame) throw new Error('Missing frame');
    const url = new URL(frame.src);
    window.dispatchEvent(new MessageEvent('message', { source: window, origin: url.origin,
      data: { protocol: 'momor-activity-frame-v1', token: url.searchParams.get('token'),
        id: url.searchParams.get('id'), sequence: 999999, at: Date.now(), source: 'forged-test', trusted: true,
        sample: { visibility: 'hidden', hidden: true, focused: false, windowBlurred: true, lifecycle: 'frozen' }, activeElement: 'body' } }));
  });
  await expect(page.locator('tbody')).not.toContainText('forged-test');
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
});

test('copied tab identities are separated before writing shared history', async ({ page, context }) => {
  await page.goto('/');
  const identifier = await page.evaluate(() => sessionStorage.getItem('momor-activity-test-tab'));
  if (!identifier) throw new Error('Missing tab identity');
  const other = await context.newPage();
  await other.addInitScript(identifier => {
    if (window === window.top) sessionStorage.setItem('momor-activity-test-tab', identifier);
  }, identifier);
  await other.goto('/');
  await expect.poll(() => other.evaluate(() => sessionStorage.getItem('momor-activity-test-tab'))).not.toBe(identifier);
  expect(await page.evaluate(() => sessionStorage.getItem('momor-activity-test-tab'))).toBe(identifier);
  await expect(other.locator('tbody')).toContainText('namespace.fork');
  await other.close();
});

test('CSV export is available', async ({ page }) => {
  await page.goto('/');
  const ready = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Exportar logs CSV' }).click();
  const download = await ready;
  expect(download.suggestedFilename()).toMatch(/\.csv$/);
  const stream = await download.createReadStream();
  if (!stream) throw new Error('Missing CSV stream');
  const chunks: Buffer[] = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk));
  expect(Buffer.concat(chunks).toString('utf8')).toContain('"isTrusted"');
});

test('active frame blur is observed even when root hasFocus remains true', async ({ page }) => {
  await page.goto('/');
  await page.getByRole('button', { name: 'Iframes', exact: true }).click();
  const frame = page.frameLocator('iframe[title="Sonda Outra origem"]');
  await frame.getByLabel('Campo do iframe').fill('A');
  await frame.locator('body').evaluate(() => window.dispatchEvent(new FocusEvent('blur')));
  await expect(page.locator('main')).toHaveAttribute('data-status', 'away');
  await expect(page.getByText('Iframe ativo perdeu foco')).toBeVisible();
  await frame.locator('body').evaluate(() => window.dispatchEvent(new FocusEvent('focus')));
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
});

test('local server heartbeat uses an independent sequence and rejects foreign origins', async ({ page, request }) => {
  await page.goto('/');
  const response = await page.waitForResponse(async response => {
    if (!response.url().endsWith('/api/activity-heartbeat') || response.status() !== 200) return false;
    const data = await response.json();
    return data.count >= 2;
  });
  const data = await response.json();
  expect(data.intervalMs).toBeGreaterThan(0);
  expect(data.maxGapMs).toBeGreaterThanOrEqual(data.intervalMs);
  const rejected = await request.post('/api/activity-heartbeat', { headers: { Origin: 'https://external.example' }, data: { id: 'test' } });
  expect(rejected.status()).toBe(403);
  const malformed = await request.post('/api/activity-heartbeat', { headers: { Origin: 'http://127.0.0.1:5173', 'Content-Type': 'application/json' }, data: '{' });
  expect(malformed.status()).toBe(400);
});

test('connection failures produce hints, not false page departures', async ({ page }) => {
  await page.route('**/api/activity-heartbeat', route => route.fulfill({ status: 503, body: '{}' }));
  await page.goto('/');
  await expect(page.getByLabel('Indicios ativos')).toContainText('network.unavailable');
  await expect(page.locator('main')).toHaveAttribute('data-status', 'active');
  await expect(page.locator('tr[data-direction="saida"]')).toHaveCount(0);
});

test('mobile fits the viewport without document overflow', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/');
  await expect(page.getByTestId('status')).toBeVisible();
  await expect(page.getByLabel('Campo de teste')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: 'artifacts/mobile-green.png', fullPage: true });
});
