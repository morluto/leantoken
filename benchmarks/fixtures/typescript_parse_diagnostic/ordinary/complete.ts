export interface Message {
  value: string;
}

export function renderMessage(message: Message): string {
  return message.value;
}

export const PRIVATE_TYPESCRIPT_SENTINEL = "retained only in source";
