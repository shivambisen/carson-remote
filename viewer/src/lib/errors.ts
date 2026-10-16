import { ApiError } from "@/api";

export function errorMessage(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === "invalid_code") return "That code is invalid or has expired. Start again on the computer.";
    if (err.code === "rate_limited") return "Too many attempts. Wait a minute and try again.";
    return err.message;
  }
  return err instanceof Error ? err.message : String(err);
}
