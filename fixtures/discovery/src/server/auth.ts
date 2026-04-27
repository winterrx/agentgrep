export function rejectUnauthorized(userId?: string) {
  if (!userId) {
    throw new Error("auth rejected");
  }
  return true;
}
