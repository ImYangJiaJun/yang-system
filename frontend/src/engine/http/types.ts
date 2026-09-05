export type SessionContext = {
  token?: string;
};

export type InvocationResult = {
  kind: "json" | "download" | "preview" | "redirect";
  status: number;
  durationMs: number;
  requestId?: string;
  message?: string;
  data?: unknown;
  blobUrl?: string;
  filename?: string;
  location?: string;
};
