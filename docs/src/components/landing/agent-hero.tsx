'use client';

import { Section, MarketingGrid, Tabs } from './compat';
import { useId } from 'react';
import { PRODUCTS, useProduct, type Product } from './product-context';
import { cn } from '@/lib/utils';
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

/**
 * Split product headlines into the single word that changes and the shared
 * remainder. Reserving only the changing word keeps the rest of the heading
 * anchored when the product changes. Returns null when the copies share no
 * common tail or the changing part is not one word per product.
 */
function splitChangingWord(headlines: string[]) {
  if (new Set(headlines).size < 2) return null;
  const tokens = headlines.map((headline) => headline.trim().split(/\s+/));
  const last = tokens[0].length;
  let shared = 0;
  while (
    shared < last - 1 &&
    tokens.every(
      (words) =>
        words.length - shared > 1 &&
        words[words.length - 1 - shared] === tokens[0][last - 1 - shared]
    )
  ) {
    shared += 1;
  }
  if (shared === 0) return null;
  const prefixes = tokens.map((words) => words.slice(0, words.length - shared));
  if (prefixes.some((words) => words.length !== 1)) return null;
  return {
    words: prefixes.map((words) => words[0]),
    suffix: tokens[0].slice(last - shared).join(' '),
  };
}

export function AgentHero({ description }: { description: string }) {
  const { selected, products } = useProduct();
  const id = useId();
  const headlineParts = splitChangingWord(
    PRODUCTS.map((product) => products[product].headline)
  );

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
              <h1 id={`${id}-heading`} className="m-0 col-start-1 row-start-1">
                {headlineParts ? (
                  <>
                    <span className="inline-grid align-baseline">
                      {PRODUCTS.map((product, index) => {
                        const active = product === selected;
                        return (
                          <span
                            key={product}
                            aria-hidden={active ? undefined : true}
                            className={cn(
                              'col-start-1 row-start-1 whitespace-nowrap',
                              !active && 'invisible'
                            )}
                          >
                            {headlineParts.words[index]}
                          </span>
                        );
                      })}
                    </span>{' '}
                    {headlineParts.suffix}
                  </>
                ) : (
                  <span className="grid">
                    {PRODUCTS.map((product) => {
                      const active = product === selected;
                      return (
                        <span
                          key={product}
                          aria-hidden={active ? undefined : true}
                          className={cn('col-start-1 row-start-1', !active && 'invisible')}
                        >
                          {products[product].headline}
                        </span>
                      );
                    })}
                  </span>
                )}
              </h1>
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
