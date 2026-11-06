'use client';

import { Button } from '@vercel/geistdocs/components/button';
import type { ComponentProps } from 'react';
import { cn } from '@/lib/utils';

type Props = Omit<ComponentProps<typeof Button>, 'size' | 'shape' | 'variant'> & {
  variant?: 'default' | 'secondary';
};

/** Only the large, pill-shaped primary/secondary actions used by this homepage. */
export function ActionButton({ className, variant = 'default', ...props }: Props) {
  return (
    <Button
      {...props}
      variant={variant}
      size="large"
      shape="rounded"
      data-landing-action={variant}
      className={cn('landing-action', className)}
    />
  );
}
