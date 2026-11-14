/** Filled check used by the decorative desktop and sign-in illustrations. */
export function CheckCircleFill({ size = 16 }: { size?: number }) {
  return (
    <svg aria-hidden="true" width={size} height={size} viewBox="0 0 16 16">
      <path
        fill="currentColor"
        fillRule="evenodd"
        clipRule="evenodd"
        d="M15 8A7 7 0 1 1 1 8a7 7 0 0 1 14 0ZM6.75 11.3 3.95 8.5l1.1-1.1 1.7 1.7 4.2-4.2 1.1 1.1Z"
      />
    </svg>
  );
}
