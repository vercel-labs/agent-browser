import { randomBytes } from 'node:crypto';

interface PendingConfirmation {
  id: string;
  action: string;
  category: string;
  description: string;
  command: Record<string, unknown>;
  timer: ReturnType<typeof setTimeout>;
}

const AUTO_DENY_TIMEOUT_MS = 60_000;

const pending = new Map<string, PendingConfirmation>();

function generateId(): string {
  return `c_${randomBytes(8).toString('hex')}`;
}

export function requestConfirmation(
  action: string,
  category: string,
  description: string,
  command: Record<string, unknown>
): { confirmationId: string } {
  const id = generateId();

  const timer = setTimeout(() => {
    pending.delete(id);
  }, AUTO_DENY_TIMEOUT_MS);

  pending.set(id, {
    id,
    action,
    category,
    description,
    command,
    timer,
  });

  return { confirmationId: id };
}

export function getAndRemovePending(
  id: string
): { command: Record<string, unknown>; action: string } | null {
  const entry = pending.get(id);
  if (!entry) return null;

  clearTimeout(entry.timer);
  pending.delete(id);
  return { command: entry.command, action: entry.action };
}
