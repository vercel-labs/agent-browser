import { cn } from "@/lib/utils";
import styles from "./brand-mark.module.css";

const marks = {
  agent: styles.agent,
  claude: styles.claude,
  openai: styles.openai,
  gemini: styles.gemini,
  cursor: styles.cursor,
};

/** Standalone public brand artwork; no design-system package is required. */
export function BrandMark({
  name,
  size = 16,
  className,
}: {
  name: keyof typeof marks;
  size?: number;
  className?: string;
}) {
  return (
    <span
      aria-hidden="true"
      className={cn(styles.mark, marks[name], className)}
      style={{
        width: size,
        height: size,
      }}
    />
  );
}
