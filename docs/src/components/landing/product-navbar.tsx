'use client';

import { Navbar } from '@vercel/geistdocs/navbar';
import { config } from '@/lib/geistdocs/config';
import { docsNav } from '@/lib/geistdocs/nav';
import { useProduct } from './product-context';

/**
 * Marketing navbar whose Docs link follows the hero product switcher. Docs
 * pages render the shared navbar from `config`, so they keep the browser docs
 * fallback.
 */
export function ProductNavbar() {
  const { product } = useProduct();
  return <Navbar config={{ ...config, nav: docsNav(product.docsHref) }} />;
}
