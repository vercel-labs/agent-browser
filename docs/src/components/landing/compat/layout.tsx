import type { ComponentPropsWithoutRef } from 'react';
import { cn } from '@/lib/utils';

/** Homepage page width and gutter, with a container for section breakpoints. */
export function Container({ className, ...props }: ComponentPropsWithoutRef<'div'>) {
  return <div className={cn('landing-container @container', className)} {...props} />;
}

/** The hero and tweet-heading grid variants currently used by the homepage. */
export function MarketingGrid({
  align,
  rowGap,
  className,
  ...props
}: ComponentPropsWithoutRef<'div'> & {
  align: 'center' | 'end';
  rowGap: 8 | 14;
}) {
  return (
    <div
      className={cn(
        'grid grid-cols-12 gap-x-6',
        align === 'center' ? 'items-center' : 'items-end',
        rowGap === 14 ? 'gap-y-14' : 'gap-y-8',
        className,
      )}
      {...props}
    />
  );
}
