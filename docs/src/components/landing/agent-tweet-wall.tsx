import { Fragment } from 'react';
import { Section, SectionHeading, MarketingGrid } from './compat';
import { TweetRail } from './tweet-rail';

export interface AgentTweetWallProps {
  heading: string;
  posts: {
    id: string;
    excerpt: string[];
    name: string;
    handle: string;
    avatar: string;
  }[];
}

export function AgentTweetWall({ heading, posts }: AgentTweetWallProps) {
  return (
    <Section top={96} bottom={false} gap={96} data-element="AgentTweetWall">
      <MarketingGrid align="end" rowGap={8}>
        <SectionHeading className="col-span-full @lg:col-start-1 @lg:col-end-8 @lg:self-baseline-last">
          {heading}
        </SectionHeading>
      </MarketingGrid>
      <TweetRail>
        {posts.map(({ id, excerpt, name, handle, avatar }) => (
          <article key={id} data-tweet-id={id} className="h-full">
            <a
              href={`https://x.com/${handle}/status/${id}`}
              target="_blank"
              rel="noopener noreferrer"
              className="group flex h-full flex-col gap-8 rounded-sm outline-hidden focus-visible:outline-2 focus-visible:outline-solid focus-visible:outline-offset-4 focus-visible:outline-(--ds-focus-color)"
            >
              <p dir="auto" className="text-copy-18 text-gray-1000">
                {excerpt.map((part, index) => (
                  <Fragment key={index}>
                    {index > 0 && (
                      <>
                        {' '}
                        <span className="-mr-[0.12em] tracking-[0.12em] text-gray-900">...</span>{' '}
                      </>
                    )}
                    {part.split(/(@\w+|#\w+|_[^_\s][^_]*_)/).map((token, tokenIndex) =>
                      tokenIndex % 2 === 0 ? (
                        token
                      ) : token.startsWith('_') ? (
                        <em key={tokenIndex}>{token.slice(1, -1)}</em>
                      ) : (
                        <span key={tokenIndex} data-tweet-mention className="font-[450]">
                          {token}
                        </span>
                      )
                    )}
                  </Fragment>
                ))}
              </p>
              <div className="mt-auto flex min-w-0 items-center gap-3">
                <img
                  src={avatar}
                  alt=""
                  width={36}
                  height={36}
                  loading="lazy"
                  decoding="async"
                  className="size-9 shrink-0 rounded-full bg-gray-200 grayscale outline outline-1 -outline-offset-1 outline-[color-mix(in_srgb,var(--ds-gray-1000)_4%,transparent)]"
                />
                <div className="min-w-0">
                  <p className="truncate text-label-14 text-gray-1000">{name}</p>
                  <p className="truncate text-label-13 text-gray-900">@{handle}</p>
                </div>
              </div>
            </a>
          </article>
        ))}
      </TweetRail>
    </Section>
  );
}
