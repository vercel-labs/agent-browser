import assert from "node:assert/strict";
import { test } from "node:test";
import {
  HERO_PRODUCT_SELECTED_EVENT,
  heroProductSelectedEvent,
} from "../src/components/landing/hero-product-event.ts";

test("names the target and previous product on a real switch", () => {
  assert.equal(HERO_PRODUCT_SELECTED_EVENT, "homepage_hero_product_selected");
  assert.deepEqual(heroProductSelectedEvent("desktop", "browser"), {
    product: "desktop",
    previous_product: "browser",
  });
  assert.deepEqual(heroProductSelectedEvent("browser", "desktop"), {
    product: "browser",
    previous_product: "desktop",
  });
});

test("ignores the product that is already selected", () => {
  assert.equal(heroProductSelectedEvent("browser", "browser"), null);
  assert.equal(heroProductSelectedEvent("desktop", "desktop"), null);
});

test("carries only primitive product names", () => {
  const event = heroProductSelectedEvent("desktop", "browser");
  assert.ok(event);
  assert.deepEqual(Object.keys(event).sort(), [
    "previous_product",
    "product",
  ]);
  for (const value of Object.values(event)) {
    assert.equal(typeof value, "string");
    assert.match(value, /^(browser|desktop)$/);
  }
});
