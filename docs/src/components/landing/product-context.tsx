'use client';

import { createContext, useContext, useState, type ReactNode } from 'react';
import { cn } from '@/lib/utils';

export type Product = 'browser' | 'desktop';
export const PRODUCTS: Product[] = ['browser', 'desktop'];
export type ProductContent = {
  headline: string;
  prompt: string;
  docsHref: string;
};
type ProductState = {
  selected: Product;
  setSelected: (product: Product) => void;
  product: ProductContent;
  products: Record<Product, ProductContent>;
};
const ProductContext = createContext<ProductState | null>(null);

export function ProductProvider({
  children,
  browser,
  desktop,
  defaultProduct = 'browser',
}: {
  children: ReactNode;
  browser: ProductContent;
  desktop: ProductContent;
  defaultProduct?: Product;
}) {
  const [selected, setSelected] = useState<Product>(defaultProduct);
  return (
    <ProductContext.Provider
      value={{
        selected,
        setSelected,
        product: selected === 'browser' ? browser : desktop,
        products: { browser, desktop },
      }}
    >
      {children}
    </ProductContext.Provider>
  );
}

export function useProduct() {
  const context = useContext(ProductContext);
  if (!context) throw new Error('Product controls require ProductProvider');
  return context;
}

/**
 * Nav product name shared by every navbar. Follows the homepage hero switcher
 * when a ProductProvider is mounted, and falls back to agent-browser elsewhere
 * (docs pages have no hero switcher).
 */
export function ProductWordmark() {
  const selected = useContext(ProductContext)?.selected ?? 'browser';
  return (
    <span className="inline-grid font-[family-name:var(--font-geist-pixel-square)] text-lg leading-none tracking-tight">
      {PRODUCTS.map((product) => {
        const active = product === selected;
        return (
          <span
            key={product}
            aria-hidden={active ? undefined : true}
            className={cn('col-start-1 row-start-1 whitespace-nowrap', !active && 'invisible')}
          >
            agent-{product}
          </span>
        );
      })}
    </span>
  );
}
