import { z } from "zod";
import { ContractError } from "./ui-catalog";

const organizationSchema = z.object({
  id: z.number().int().positive(),
  name: z.string().trim().min(1),
  code: z.string().trim().min(1),
});

const organizationPageSchema = z.object({
  items: z.array(organizationSchema),
  total: z.number().int().nonnegative().optional(),
  page: z.number().int().positive().optional(),
  limit: z.number().int().positive().optional(),
  total_pages: z.number().int().nonnegative().optional(),
});

export type OrganizationSummary = z.infer<typeof organizationSchema>;

export function parseOrganizationsPage(
  payload: unknown,
): OrganizationSummary[] {
  const parsed = organizationPageSchema.safeParse(payload);
  if (!parsed.success) {
    throw new ContractError(
      "企业账号列表契约校验失败",
      parsed.error.issues.map(
        (issue) => `${issue.path.join(".") || "<root>"}: ${issue.message}`,
      ),
    );
  }
  return parsed.data.items;
}
