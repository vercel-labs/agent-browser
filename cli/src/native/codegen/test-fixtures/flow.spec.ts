import { test, expect } from '@playwright/test';

test.use({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true });

test("hostile \"flow\"\nname", async ({ page, context }) => {
  await page.goto("https://example.com/start\nnext");
  await page.frameLocator('iframe, frame').nth(0).frameLocator('iframe, frame').nth(1).locator("xpath=//input[@name='q']").fill("line one\nline two");
  await page.locator("#type").fill("");
  await page.locator("#type").pressSequentially("suffix", { delay: 17 });
  await page.locator("select").selectOption(["a","b"]);
  await page.keyboard.press("Control+Shift+A");
  await expect(page).toHaveURL("https://example.com/done");
  await page.locator("#scroll").evaluate((element, delta) => element.scrollBy(delta.x, delta.y), { x: 3, y: 9 });
  await page.locator("input[type=file]").setInputFiles(["one.txt","two.txt"]);
  const page2 = await context.newPage();
  await page2.goto("https://example.com/same");
  const page3 = await context.newPage();
  await page3.goto("https://example.com/same");
  await page.getByRole("button", { name: "Save \"now\"", exact: true }).hover();
  await page2.close();
});

