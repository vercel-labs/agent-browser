'use client';

import { ActionButton, IconCheck, IconCopy, BrandMark } from './compat';
import { cn } from '@/lib/utils';
import { useEffect, useRef, useState } from 'react';
import { useProduct } from './product-context';

// Keep the copy feedback within the button’s existing footprint.
const iconCrossfade =
  'absolute inset-0 flex items-center justify-center transition-[opacity,scale] duration-200 ease-[cubic-bezier(0.23,1,0.32,1)] motion-reduce:transition-none';

// Overlap the agent marks in three small, monochrome circles.
function AgentLogoStack() {
  const badge =
    'flex size-6 items-center justify-center rounded-full bg-gray-1000 text-background-100 shadow-[inset_0_0_0_1px_color-mix(in_srgb,currentColor_25%,transparent)]';
  return (
    <span aria-hidden className="relative inline-flex h-6 w-14 shrink-0">
      <span className={cn(badge, 'absolute left-0 z-0')}>
        <BrandMark name="claude" size={14} />
      </span>
      <span className={cn(badge, 'absolute left-4 z-10')}>
        <BrandMark name="openai" size={14} />
      </span>
      <span className={cn(badge, 'absolute left-8 z-20')}>
        <BrandMark name="cursor" size={14} />
      </span>
    </span>
  );
}

function CopyGlyph({ copied }: { copied: boolean }) {
  return (
    <span aria-hidden className="relative flex size-4 shrink-0 opacity-70 ml-1.5">
      <span className={cn(iconCrossfade, copied ? 'scale-50 opacity-0' : 'scale-100 opacity-100')}>
        <IconCopy size={16} />
      </span>
      <span className={cn(iconCrossfade, copied ? 'scale-100 opacity-100' : 'scale-50 opacity-0')}>
        <IconCheck size={16} />
      </span>
    </span>
  );
}

function CopyPromptButton({
  prompt,
  label,
  showAgentIcons,
}: {
  prompt: string;
  label: string;
  showAgentIcons: boolean;
}) {
  const [status, setStatus] = useState<'idle' | 'copied' | 'error'>('idle');
  const attempt = useRef(0);
  useEffect(
    () => () => {
      attempt.current += 1;
    },
    []
  );
  useEffect(() => {
    if (status === 'idle') return;
    const timer = setTimeout(() => setStatus('idle'), 2000);
    return () => clearTimeout(timer);
  }, [status]);

  async function copy() {
    const current = ++attempt.current;
    try {
      await navigator.clipboard.writeText(prompt);
      if (current === attempt.current) setStatus('copied');
    } catch {
      if (current === attempt.current) setStatus('error');
    }
  }

  return (
    <>
      <ActionButton
        aria-label={
          status === 'copied'
            ? 'Paste to your agent'
            : status === 'error'
              ? 'Try copying again'
              : label
        }
        onClick={copy}
        variant="default"
        className={cn('group', showAgentIcons && 'pl-1! pr-2!')}
      >
        <span className="flex items-center gap-2">
          {showAgentIcons && <AgentLogoStack />}
          <span className="grid place-items-center">
            <span aria-hidden className="invisible col-start-1 row-start-1 whitespace-nowrap">
              {label}
            </span>
            <span className="col-start-1 row-start-1 whitespace-nowrap">
              {status === 'copied'
                ? 'Paste to your agent'
                : status === 'error'
                  ? 'Try again'
                  : label}
            </span>
          </span>
          <CopyGlyph copied={status === 'copied'} />
        </span>
      </ActionButton>
      <span role="status" className="sr-only">
        {status === 'copied'
          ? 'Prompt copied. Paste it into your coding agent.'
          : status === 'error'
            ? 'Could not copy the prompt. Try again.'
            : ''}
      </span>
    </>
  );
}

export function ProductActions({
  centered = false,
  showAgentIcons = true,
  showDocsLink = true,
  installLabel = 'Teach your agent',
}: {
  centered?: boolean;
  showAgentIcons?: boolean;
  showDocsLink?: boolean;
  installLabel?: string;
}) {
  const { selected, product } = useProduct();
  return (
    <div
      className={cn('flex flex-wrap items-center gap-3', centered && 'justify-center')}
      data-product-actions={selected}
    >
      <CopyPromptButton
        key={selected}
        prompt={product.prompt}
        label={installLabel}
        showAgentIcons={showAgentIcons}
      />
      {showDocsLink && (
        <ActionButton Component="a" variant="secondary" href={product.docsHref}>
          Read the docs
        </ActionButton>
      )}
    </div>
  );
}
