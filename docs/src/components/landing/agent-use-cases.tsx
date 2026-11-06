import type { ReactNode } from 'react';
import { Section, SectionHeading } from './compat';
import { UseCaseVisual, type UseCaseVisualKind } from './use-case-visual';
import { SceneSequence, SceneSequenceItem } from './use-case-scenes/scene-sequence';

export interface AgentUseCasesProps {
  heading: string;
  items: { title: string; visual: UseCaseVisualKind; description: string }[];
}

/** One key phrase per description, marked **like this**, set in ink at 450. */
function withHighlight(description: string): ReactNode {
  const [before, phrase, after] = description.split('**');
  if (phrase === undefined) return description;
  return <>{before}<strong className="font-[450] text-gray-1000">{phrase}</strong>{after}</>;
}

export function AgentUseCases({ heading, items }: AgentUseCasesProps) {
  const scenes = items.filter(({ visual }) => visual === 'sign-in' || visual === 'performance' || visual === 'verify');
  return (
    <Section top={120} bottom={120} gap={72} data-element="AgentUseCases">
      <SectionHeading className="max-w-[28ch]">{heading}</SectionHeading>
      <SceneSequence count={scenes.length} className="m-0 grid list-none gap-x-6 gap-y-16 p-0 @md:grid-cols-3">
        {items.map((item) => (
          <li key={item.title} className="flex flex-col">
            <SceneSequenceItem index={scenes.indexOf(item)}>
              <div className="overflow-hidden rounded-[2px] border border-gray-alpha-400 bg-background-100">
                <UseCaseVisual kind={item.visual} />
              </div>
            </SceneSequenceItem>
            <h3 className="mt-6 text-[20px] leading-6 font-[450] text-gray-1000">{item.title}</h3>
            <p className="mt-2 text-[16px] leading-6 font-normal text-gray-900">{withHighlight(item.description)}</p>
          </li>
        ))}
      </SceneSequence>
    </Section>
  );
}
