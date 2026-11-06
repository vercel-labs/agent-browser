import { Section, SectionHeading } from './compat';
import { ProductActions } from './product-actions';

export function AgentClosing({ headline }: { headline: string }) {
  return (
    <Section top={168} bottom={168} gap={false} data-element="AgentClosing">
      <div className="flex min-h-[300px] flex-col items-center justify-center gap-8 text-center @lg:min-h-[360px] @lg:gap-10">
        <SectionHeading size="64" className="max-w-[18ch] whitespace-pre-line">
          {headline}
        </SectionHeading>
        <ProductActions centered />
      </div>
    </Section>
  );
}
