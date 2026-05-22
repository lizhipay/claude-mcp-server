export function maskSecretForDisplay(value: string): string {
  if (!value) return "还没有填写密钥哦";
  if (value.length <= 8) return "••••";
  return `${value.slice(0, 4)}…${value.slice(-4)}`;
}
