import type { Product } from './product-context';

export const HERO_PRODUCT_SELECTED_EVENT = 'homepage_hero_product_selected';

export type HeroProductSelectedEvent = {
  product: Product;
  previous_product: Product;
};

/**
 * Builds the custom event payload for a hero product switch, or null when the
 * selection did not change. The Geistdocs tabs select on focus, so a pointer or
 * keyboard interaction can report the already-selected product; only a real
 * switch should count as a product selection.
 */
export function heroProductSelectedEvent(
  product: Product,
  previous: Product
): HeroProductSelectedEvent | null {
  if (product === previous) return null;
  return { product, previous_product: previous };
}
