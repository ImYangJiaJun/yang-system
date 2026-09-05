import { ContractError } from "@/engine/contracts/ui-catalog";
import type { SessionContext } from "./types";

export const apiBase = (import.meta.env.VITE_API_BASE_URL ?? "").replace(
  /\/$/,
  "",
);

export function contextHeaders(context: SessionContext): Headers {
  const headers = new Headers({ Accept: "application/json" });
  if (context.token?.trim())
    headers.set("Authorization", `Bearer ${context.token.trim()}`);
  return headers;
}

export async function parseJson(response: Response): Promise<unknown> {
  const text = await response.text();
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch (error) {
    throw new ContractError("服务端返回了无效 JSON", [
      error instanceof Error ? error.message : String(error),
    ]);
  }
}
