import { useRef } from "react";
import { useSearchParams } from "react-router";
import { hasOperation, useUiCatalog } from "@/engine";
import { Button } from "@/shared/ui/button";
import { APPROVAL_OPERATION_IDS } from "../api";
import ApprovalConfigsPage from "./ApprovalConfigsPage";
import ApprovalRequestsPage from "./ApprovalRequestsPage";

export default function ApprovalConsolePage() {
  const { data: catalog } = useUiCatalog();
  const [params, setParams] = useSearchParams();
  const refs = useRef(new Map<string, HTMLButtonElement>());
  const tabs = [
    {
      id: "configs",
      title: "配置",
      operation: APPROVAL_OPERATION_IDS.listConfigs,
    },
    {
      id: "requests",
      title: "派发记录",
      operation: APPROVAL_OPERATION_IDS.listRequests,
    },
  ].filter((tab) => hasOperation(catalog, tab.operation));
  const selected = tabs.find((tab) => tab.id === params.get("tab")) ?? tabs[0];
  const select = (id: string) => {
    const next = new URLSearchParams(params);
    next.set("tab", id);
    setParams(next);
  };
  if (!selected)
    return (
      <p className="p-6" aria-live="polite">
        当前功能域没有审批控制台的读取权限。请联系管理员。
      </p>
    );
  return (
    <div>
      <div
        role="tablist"
        aria-label="审批控制台"
        className="flex gap-2 border-b p-4"
      >
        {tabs.map((tab, index) => (
          <Button
            key={tab.id}
            ref={(node) => {
              if (node) refs.current.set(tab.id, node);
              else refs.current.delete(tab.id);
            }}
            role="tab"
            id={`approval-tab-${tab.id}`}
            aria-controls={`approval-panel-${tab.id}`}
            aria-selected={selected.id === tab.id}
            tabIndex={selected.id === tab.id ? 0 : -1}
            variant={selected.id === tab.id ? "default" : "ghost"}
            onClick={() => select(tab.id)}
            onKeyDown={(event) => {
              const nextIndex =
                event.key === "Home"
                  ? 0
                  : event.key === "End"
                    ? tabs.length - 1
                    : event.key === "ArrowRight"
                      ? (index + 1) % tabs.length
                      : event.key === "ArrowLeft"
                        ? (index + tabs.length - 1) % tabs.length
                        : null;
              if (nextIndex === null) return;
              event.preventDefault();
              const next = tabs[nextIndex];
              if (next) {
                select(next.id);
                refs.current.get(next.id)?.focus();
              }
            }}
          >
            {tab.title}
          </Button>
        ))}
      </div>
      <section
        role="tabpanel"
        id={`approval-panel-${selected.id}`}
        aria-labelledby={`approval-tab-${selected.id}`}
        tabIndex={0}
      >
        {selected.id === "configs" ? (
          <ApprovalConfigsPage />
        ) : (
          <ApprovalRequestsPage />
        )}
      </section>
    </div>
  );
}
