'use client';

import { createContext, useContext, useState, type ReactNode } from 'react';

export type Product = 'browser' | 'desktop';
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

export function ProductWordmark() {
  const { selected } = useProduct();
  return (
    <span className="font-[family-name:var(--font-geist-pixel-square)] text-lg leading-none tracking-tight">
      agent-{selected}
    </span>
  );
}
