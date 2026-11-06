import type { ComponentPropsWithoutRef } from "react";
import { cn } from "@/lib/utils";

const padding = {
  72: "pt-12 @md:pt-16 @lg:pt-18",
  96: "pt-16 @md:pt-20 @lg:pt-24",
  120: "pt-18 @md:pt-24 @lg:pt-30",
  168: "pt-20 @md:pt-30 @lg:pt-42",
};
const bottomPadding = {
  120: "pb-18 @md:pb-24 @lg:pb-30",
  168: "pb-20 @md:pb-30 @lg:pb-42",
};
const gaps = {
  72: "gap-12 @md:gap-16 @lg:gap-18",
  96: "gap-16 @md:gap-20 @lg:gap-24",
};

/** Spacing shared by the four homepage sections. */
export function Section({
  top,
  bottom,
  gap,
  className,
  ...props
}: ComponentPropsWithoutRef<"section"> & {
  top: keyof typeof padding;
  bottom: keyof typeof bottomPadding | false;
  gap: keyof typeof gaps | false;
}) {
  return (
    <section
      className={cn(
        "relative flex flex-col",
        padding[top],
        bottom && bottomPadding[bottom],
        gap && gaps[gap],
        className,
      )}
      {...props}
    />
  );
}

export function SectionHeading({
  size = "56",
  className,
  ...props
}: ComponentPropsWithoutRef<"h2"> & { size?: "56" | "64" }) {
  return (
    <h2
      data-size={size}
      className={cn(
        "landing-heading text-gray-1000",
        className,
      )}
      {...props}
    />
  );
}
