interface ComponentProps {
  label: string;
}

export function Component({ label }: ComponentProps) {
  return <section aria-label={label}>{label}</section>;
}
