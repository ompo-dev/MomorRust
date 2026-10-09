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
  await expect(page.getByRole('cell', { name: 'pagehide', exact: true })).toHaveCount(1);
  const event = page.locator('tr').filter({ has: page.getByRole('cell', { name: 'pagehide', exact: true }) });
  await expect(event).toContainText('Nativo');
  await expect(page.getByRole('cell', { name: 'inicio', exact: true })).toHaveCount(2);
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
  expect(data.version).toBe(1);
  expect(data.exits).toBe(1);
  expect(data.events.some((event: { direction: string }) => event.direction === 'entrada')).toBe(true);
  await page.getByRole('button', { name: 'Limpar logs' }).click();
  await expect(page.locator('tbody tr')).toHaveCount(1);
  await expect(page.locator('tbody')).toContainText('limpeza');
  await page.reload();
  await expect(page.locator('tbody')).toContainText('limpeza');
});

test('mobile fits the viewport without document overflow', async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto('/');
  await expect(page.getByTestId('status')).toBeVisible();
  await expect(page.getByLabel('Campo de teste')).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.screenshot({ path: 'artifacts/mobile-green.png', fullPage: true });
});
