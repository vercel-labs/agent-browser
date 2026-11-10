'use client';

import { Section, MarketingGrid, Tabs } from './compat';
import { useId } from 'react';
import { useProduct, type Product } from './product-context';
import { ProductActions } from './product-actions';
import { HeroBrowserVisual } from './hero-browser-visual';

function ProductSwitcher({ id }: { id: string }) {
  const { selected, setSelected } = useProduct();
  return (
    <div className="relative h-10 w-fit max-w-full rounded-full bg-gray-100 p-1">
      <span
        aria-hidden="true"
        className={`pointer-events-none absolute top-1 bottom-1 left-1 w-[calc((100%-8px)/2)] rounded-full bg-background-100 shadow-xs transition-transform duration-200 ease-out will-change-transform motion-reduce:transition-none ${selected === 'desktop' ? 'translate-x-full' : 'translate-x-0'}`}
      />
      <Tabs<Product>
        aria-label="Choose an agent tool"
        className="relative m-0! grid! h-8 grid-cols-2 gap-0 overflow-visible! p-0!"
        tabClassName="m-0! rounded-full! h-8! justify-center px-4! py-0! font-[family-name:var(--font-geist-sans)]! text-[14px]! font-medium! aria-selected:bg-transparent!"
        variant="secondary"
        selected={selected}
        setSelected={setSelected}
        tabs={(['browser', 'desktop'] as const).map((key) => ({
          title: key === 'browser' ? 'Browser' : 'Desktop',
          value: key,
          id: `${id}-${key}`,
          panelId: `${id}-panel`,
        }))}
      />
    </div>
  );
}

export function AgentHero({ description }: { description: string }) {
  const { selected, products } = useProduct();
  const id = useId();

  return (
    <Section top={72} bottom={120} gap={false} className="text-gray-1000" aria-label="Tools for agents">
      <MarketingGrid align="center" rowGap={14} className="@lg:min-h-[500px] @lg:grid-cols-11">
        <div className="col-span-full min-w-0 @lg:col-start-1 @lg:col-end-6 @lg:max-w-[33.5rem]">
          <ProductSwitcher id={id} />
          <div
            id={`${id}-panel`}
            role="tabpanel"
            aria-labelledby={`${id}-${selected}`}
            tabIndex={0}
            className="mt-6 outline-none focus-visible:outline-2 focus-visible:outline-offset-8 focus-visible:outline-(--ds-focus-color)"
          >
            <div className="grid max-w-[15ch] text-heading-40 @md:text-heading-64 text-balance">
              <h1 id={`${id}-heading`} className="m-0 col-start-1 row-start-1">{products[selected].headline}</h1>
            </div>
            <p className="mt-6 max-w-[46ch] text-copy-18 text-gray-900 text-pretty">
              {description}
            </p>
            <div className="mt-6 @md:mt-8">
              <ProductActions showDocsLink={false} />
            </div>
          </div>
        </div>
        <div className="col-span-full min-w-0 @lg:col-start-6 @lg:col-end-12">
          <HeroBrowserVisual product={selected} />
        </div>
      </MarketingGrid>
    </Section>
  );
}
