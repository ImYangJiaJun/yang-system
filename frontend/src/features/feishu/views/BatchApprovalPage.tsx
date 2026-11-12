import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type HTMLAttributes,
  type ReactNode,
  type SyntheticEvent,
} from "react";
import { Link, useNavigate } from "react-router";
import {
  ConfigProvider,
  Table,
  Tag,
  Tooltip,
  type TableColumnsType,
} from "antd";
import zhCN from "antd/locale/zh_CN";
import {
  ChevronLeft,
  ChevronRight,
  Download,
  Eye,
  Inbox,
  RefreshCw,
} from "lucide-react";
import { Resizable, type ResizeCallbackData } from "react-resizable";
import * as XLSX from "xlsx";
import {
  PDFWorker,
  getDocument,
  type PDFDocumentLoadingTask,
  type PDFDocumentProxy,
  type RenderTask,
} from "pdfjs-dist";
import PdfWorker from "pdfjs-dist/build/pdf.worker.min.mjs?worker";

import { apiBase, useSessionController, useSessionCredentials } from "@/engine";
import { Button } from "@/shared/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Skeleton } from "@/shared/ui/skeleton";
import { formatProductDateTime } from "@/shared/lib/product-locale";
import { useToast } from "@/shared/lib/toast";

import {
  BATCH_ACTION_LABELS,
  createBatchJob,
  describeBatchError,
  fetchBatchTaskRows,
  fetchBatchTasks,
  fetchCurrentUsername,
  groupBatchTasks,
  isFeishuReauthError,
  isFeishuScopeError,
  newIdempotencyKey,
  requestFeishuAuthorizationUrl,
  taskOperability,
  taskStatusLabel,
  type BatchApprovalAction,
  type BatchJobItem,
  type BatchTask,
  type BatchTaskDetailCell,
  type BatchTaskDetailColumn,
  type BatchTaskFormField,
  type BatchViewConfig,
} from "../batch-approval-api";
import {
  BatchActionDialog,
  type BatchActionParams,
} from "../components/BatchActionDialog";
import { BatchJobDialog } from "../components/BatchJobDialog";
import { BatchViewSettingsDrawer } from "../components/BatchViewSettingsDrawer";
import { StatusBadge } from "../components/StatusBadge";

function timeText(value: number | null): string {
  return value === null ? "—" : formatProductDateTime(value);
}

function dateKey(value: number | null): string {
  if (value === null) return "";
  const date = new Date(value);
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

function waitingText(createdAt: number | null): {
  text: string;
  className: string;
} | null {
  if (createdAt === null) return null;
  const hours = Math.floor(Math.max(0, Date.now() - createdAt) / 3_600_000);
  if (hours < 24) {
    return {
      text: hours < 1 ? "刚提交" : `已等待 ${hours} 小时`,
      className: "text-muted-foreground",
    };
  }
  const days = Math.floor(hours / 24);
  return {
    text: `已等待 ${days} 天`,
    className: days >= 3 ? "text-destructive" : "text-tone-warning",
  };
}

function statusTone(label: string): "info" | "positive" | "danger" | "neutral" {
  if (label === "待办" || label === "处理中" || label === "审批中") {
    return "info";
  }
  if (
    label === "已办" ||
    label === "已通过" ||
    label === "已完成" ||
    label === "已读"
  ) {
    return "positive";
  }
  if (label === "已拒绝" || label === "撤回" || label === "已撤销") {
    return "danger";
  }
  return "neutral";
}

type SelectedMap = Record<string, BatchTask>;

type LastJobRequest = {
  action: BatchApprovalAction;
  params: BatchActionParams;
  items: BatchJobItem[];
};

function fieldValueText(value: unknown): string {
  if (value === null || value === undefined) return "";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  if (Array.isArray(value)) {
    return value.map(fieldValueText).filter(Boolean).join("、");
  }
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    const preferred =
      record.text ?? record.name ?? record.label ?? record.value;
    if (typeof preferred === "string" || typeof preferred === "number") {
      return String(preferred);
    }
    return JSON.stringify(value);
  }
  return String(value);
}

type AttachmentKind = "image" | "pdf" | "office" | "other";

type AttachmentFile = {
  name: string;
  url: string;
  kind: AttachmentKind;
};

function attachmentNames(ext: unknown): string[] {
  if (Array.isArray(ext)) {
    return ext.filter((item): item is string => typeof item === "string");
  }
  if (typeof ext !== "string") return [];
  const text = ext.trim();
  if (!text) return [];
  if (text.startsWith("[")) {
    try {
      const parsed = JSON.parse(text) as unknown;
      if (Array.isArray(parsed)) {
        return parsed.filter(
          (item): item is string => typeof item === "string",
        );
      }
    } catch {
      // 不是 JSON，继续按飞书的单引号列表解析。
    }
  }
  const quoted = [...text.matchAll(/'([^']*)'/g)]
    .map((match) => match[1]?.trim() ?? "")
    .filter(Boolean);
  if (quoted.length > 0) return quoted;
  return text
    .split(",")
    .map((item) => item.trim().replace(/^['"]|['"]$/g, ""))
    .filter(Boolean);
}

function attachmentObject(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const hasUrl =
    typeof record.url === "string" || typeof record.tmp_url === "string";
  const hasToken = typeof record.file_token === "string";
  const hasName =
    typeof record.name === "string" || typeof record.file_name === "string";
  return hasUrl || hasToken || hasName ? record : null;
}

function attachmentUrls(value: unknown): string[] {
  if (typeof value === "string") {
    return value
      .split(/,\s*(?=https?:\/\/)/)
      .map((item) => item.trim())
      .filter((item) => /^https?:\/\//i.test(item));
  }
  if (Array.isArray(value)) {
    return value.flatMap(attachmentUrls);
  }
  const record = attachmentObject(value);
  if (!record) return [];
  const url = record.url ?? record.tmp_url;
  return typeof url === "string" && /^https?:\/\//i.test(url) ? [url] : [];
}

function fileKind(name: string, url: string): AttachmentKind {
  const path = (() => {
    try {
      return new URL(url).pathname;
    } catch {
      return url;
    }
  })();
  const extension = `${name} ${path}`
    .toLowerCase()
    .match(/\.([a-z0-9]+)(?:\s|$)/)?.[1];
  if (
    extension &&
    ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif"].includes(
      extension,
    )
  ) {
    return "image";
  }
  if (extension === "pdf") return "pdf";
  if (extension && ["xls", "xlsx", "csv"].includes(extension)) return "office";
  return "other";
}

function urlFileName(url: string): string {
  try {
    const segment = new URL(url).pathname.split("/").filter(Boolean).pop();
    return segment ? decodeURIComponent(segment) : "";
  } catch {
    return "";
  }
}

function attachmentsOf(field: BatchTaskFormField): AttachmentFile[] {
  const fieldType = (field.fieldType ?? "").toLowerCase();
  const urls = attachmentUrls(field.value);
  if (urls.length === 0) return [];
  const looksLikeAttachment =
    fieldType.includes("attachment") ||
    fieldType.includes("image") ||
    (Array.isArray(field.value) &&
      field.value.some((item) => attachmentObject(item) !== null));
  if (!looksLikeAttachment) return [];

  const names = attachmentNames(field.ext);
  return urls.map((url, index) => {
    const name = names[index] || urlFileName(url) || `附件 ${index + 1}`;
    return {
      name,
      url,
      kind:
        fieldType.includes("image") && !/\.[a-z0-9]+$/i.test(name)
          ? "image"
          : fileKind(name, url),
    };
  });
}

function attachmentProxyUrl(file: AttachmentFile): string {
  const query = new URLSearchParams({ url: file.url, name: file.name });
  return `${apiBase}/api/v1/feishu/batch-approval/attachments?${query.toString()}`;
}

async function fetchAttachmentBlob(
  file: AttachmentFile,
  token: string | undefined,
): Promise<Blob> {
  const response = await fetch(attachmentProxyUrl(file), {
    headers: token ? { Authorization: `Bearer ${token}` } : undefined,
  });
  if (!response.ok) {
    throw new Error(`附件下载失败（HTTP ${response.status}）`);
  }
  return response.blob();
}

async function downloadAttachment(
  file: AttachmentFile,
  token: string | undefined,
): Promise<void> {
  const blob = await fetchAttachmentBlob(file, token);
  const objectUrl = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = objectUrl;
  anchor.download = file.name;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  window.setTimeout(() => URL.revokeObjectURL(objectUrl), 1000);
}

function SpreadsheetPreview({ blob }: { blob: Blob }) {
  const [sheets, setSheets] = useState<
    Array<{ name: string; rows: string[][] }>
  >([]);
  const [active, setActive] = useState(0);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void blob
      .arrayBuffer()
      .then((buffer) => {
        if (cancelled) return;
        const workbook = XLSX.read(buffer, { type: "array" });
        const parsed = workbook.SheetNames.map((name) => {
          const rows = XLSX.utils.sheet_to_json(workbook.Sheets[name], {
            header: 1,
            raw: false,
            defval: "",
          }) as unknown[][];
          return {
            name,
            rows: rows
              .slice(0, 200)
              .map((row) =>
                row
                  .slice(0, 40)
                  .map((cell) => (cell == null ? "" : String(cell))),
              ),
          };
        });
        setSheets(parsed);
        setActive(0);
      })
      .catch(() => {
        if (!cancelled) setError("表格解析失败，无法预览");
      });
    return () => {
      cancelled = true;
    };
  }, [blob]);

  if (error) {
    return (
      <p role="alert" className="text-destructive text-sm">
        {error}
      </p>
    );
  }
  if (sheets.length === 0) {
    return <Skeleton className="h-40 w-full" />;
  }
  const sheet = sheets[active] ?? sheets[0];
  return (
    <div className="flex max-h-[70vh] flex-col gap-2">
      {sheets.length > 1 && (
        <div className="flex flex-wrap gap-1">
          {sheets.map((sheet, index) => (
            <Button
              key={sheet.name}
              type="button"
              size="sm"
              variant={index === active ? "secondary" : "ghost"}
              onClick={() => setActive(index)}
            >
              {sheet.name}
            </Button>
          ))}
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-auto rounded border">
        <table className="w-full border-collapse text-xs">
          <tbody>
            {sheet.rows.length === 0 ? (
              <tr>
                <td className="p-3 text-muted-foreground">空表格</td>
              </tr>
            ) : (
              sheet.rows.map((row, rowIndex) => (
                <tr key={rowIndex} className="border-b last:border-b-0">
                  {row.map((cell, cellIndex) => (
                    <td
                      key={cellIndex}
                      className="max-w-56 truncate border-r px-2 py-1 align-top last:border-r-0"
                      title={cell}
                    >
                      {cell || " "}
                    </td>
                  ))}
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function PdfPreview({ blob }: { blob: Blob }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [pdf, setPdf] = useState<PDFDocumentProxy | null>(null);
  const [pageNumber, setPageNumber] = useState(1);
  const [zoom, setZoom] = useState(1);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let loadingTask: PDFDocumentLoadingTask | null = null;
    let worker: PDFWorker | null = null;
    setPdf(null);
    setPageNumber(1);
    setZoom(1);
    setError(null);
    void blob
      .arrayBuffer()
      .then((buffer) => {
        worker = PDFWorker.create({ port: new PdfWorker() });
        loadingTask = getDocument({
          data: new Uint8Array(buffer),
          worker,
        });
        return loadingTask.promise;
      })
      .then((loaded) => {
        if (cancelled) {
          void loadingTask?.destroy();
          return;
        }
        setPdf(loaded);
      })
      .catch(() => {
        if (!cancelled) setError("PDF 加载失败，请下载后查看");
      });
    return () => {
      cancelled = true;
      void loadingTask?.destroy();
      void worker?.destroy();
    };
  }, [blob]);

  useEffect(() => {
    if (!pdf) return;
    let cancelled = false;
    let task: RenderTask | null = null;
    void (async () => {
      const page = await pdf.getPage(pageNumber);
      if (cancelled) return;
      const viewport = page.getViewport({ scale: zoom });
      const canvas = canvasRef.current;
      const context = canvas?.getContext("2d");
      if (!canvas || !context) return;
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      task = page.render({ canvas, canvasContext: context, viewport });
      await task.promise;
    })().catch((reason: unknown) => {
      if (
        !cancelled &&
        !(
          reason instanceof Error &&
          reason.name === "RenderingCancelledException"
        )
      ) {
        setError("PDF 渲染失败，请下载后查看");
      }
    });
    return () => {
      cancelled = true;
      task?.cancel();
    };
  }, [pdf, pageNumber, zoom]);

  if (error) {
    return (
      <p role="alert" className="text-destructive text-sm">
        {error}
      </p>
    );
  }
  if (!pdf) {
    return <Skeleton className="h-full min-h-64 w-full" />;
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-2">
      <div className="flex flex-wrap items-center gap-1 text-xs">
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={pageNumber <= 1}
          onClick={() => setPageNumber((value) => Math.max(1, value - 1))}
        >
          上一页
        </Button>
        <span className="w-16 text-center tabular-nums">
          {pageNumber} / {pdf.numPages}
        </span>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={pageNumber >= pdf.numPages}
          onClick={() =>
            setPageNumber((value) => Math.min(pdf.numPages, value + 1))
          }
        >
          下一页
        </Button>
        <span className="mx-1 h-4 w-px bg-border" />
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => setZoom((value) => Math.max(0.5, value - 0.25))}
        >
          缩小
        </Button>
        <span className="w-16 text-center tabular-nums">
          {Math.round(zoom * 100)}%
        </span>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => setZoom((value) => Math.min(2.5, value + 0.25))}
        >
          放大
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          onClick={() => setZoom(1)}
        >
          适应窗口
        </Button>
      </div>
      <div className="bg-muted/30 flex min-h-0 flex-1 items-start justify-center overflow-auto rounded border p-2">
        <canvas ref={canvasRef} className="shadow-sm" />
      </div>
    </div>
  );
}

function AttachmentPreview({
  files,
  index,
  onChange,
  onClose,
}: {
  files: AttachmentFile[];
  index: number;
  onChange: (index: number) => void;
  onClose: () => void;
}) {
  const { token } = useSessionCredentials();
  const file = files[index] ?? files[0];
  const [blob, setBlob] = useState<Blob | null>(null);
  const [objectUrl, setObjectUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);

  useEffect(() => {
    let cancelled = false;
    let url: string | null = null;
    setBlob(null);
    setObjectUrl(null);
    setError(null);
    setZoom(1);
    void fetchAttachmentBlob(file, token)
      .then((next) => {
        if (cancelled) return;
        url = URL.createObjectURL(next);
        setBlob(next);
        setObjectUrl(url);
      })
      .catch(() => {
        if (!cancelled) setError("附件加载失败，请重试");
      });
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [file, token]);

  const move = (delta: number) => {
    onChange((index + delta + files.length) % files.length);
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <DialogContent className="flex h-[85vh] max-h-[85vh] flex-col gap-2 sm:max-w-6xl">
        <div className="flex items-center gap-3 pr-8">
          <DialogTitle className="min-w-0 flex-1 truncate">
            {file.name}
          </DialogTitle>
          {files.length > 1 && (
            <span className="text-muted-foreground shrink-0 text-xs tabular-nums">
              {index + 1} / {files.length}
            </span>
          )}
          {objectUrl && (
            <a
              href={objectUrl}
              download={file.name}
              className="text-primary shrink-0 text-xs underline-offset-4 hover:underline"
            >
              下载附件
            </a>
          )}
        </div>
        {error ? (
          <p role="alert" className="text-destructive text-sm">
            {error}
          </p>
        ) : !blob || !objectUrl ? (
          <Skeleton className="h-full min-h-64 w-full" />
        ) : file.kind === "image" ? (
          <>
            <div className="flex items-center gap-1 text-xs">
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => setZoom((value) => Math.max(0.25, value - 0.25))}
              >
                缩小
              </Button>
              <span className="w-16 text-center tabular-nums">
                {Math.round(zoom * 100)}%
              </span>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => setZoom((value) => Math.min(4, value + 0.25))}
              >
                放大
              </Button>
              <Button
                type="button"
                size="sm"
                variant="outline"
                onClick={() => setZoom(1)}
              >
                适应窗口
              </Button>
            </div>
            <div className="bg-muted/30 flex min-h-0 flex-1 items-center justify-center overflow-auto rounded border p-2">
              <img
                src={objectUrl}
                alt={file.name}
                className="max-h-full max-w-full object-contain"
                style={{ transform: `scale(${zoom})` }}
              />
            </div>
          </>
        ) : file.kind === "pdf" ? (
          <PdfPreview blob={blob} />
        ) : file.kind === "office" ? (
          <SpreadsheetPreview blob={blob} />
        ) : (
          <p className="text-muted-foreground text-sm">
            该格式不支持在线预览。
          </p>
        )}
        {files.length > 1 && (
          <>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              aria-label="上一个附件"
              className="absolute top-1/2 left-3 z-10 -translate-y-1/2 rounded-full bg-black/35 text-white hover:bg-black/55 hover:text-white"
              onClick={() => move(-1)}
            >
              <ChevronLeft className="size-5" />
            </Button>
            <Button
              type="button"
              size="icon"
              variant="ghost"
              aria-label="下一个附件"
              className="absolute top-1/2 right-3 z-10 -translate-y-1/2 rounded-full bg-black/35 text-white hover:bg-black/55 hover:text-white"
              onClick={() => move(1)}
            >
              <ChevronRight className="size-5" />
            </Button>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function AttachmentCell({ files }: { files: AttachmentFile[] }) {
  const { token } = useSessionCredentials();
  const { error: toastError } = useToast();
  const [preview, setPreview] = useState<{
    files: AttachmentFile[];
    index: number;
  } | null>(null);
  return (
    <div className="flex max-w-64 flex-col gap-1">
      {files.map((file, index) => (
        <div
          key={`${file.url}-${index}`}
          className="flex items-center gap-1 rounded border px-1.5 py-1 text-xs"
        >
          <span className="min-w-0 flex-1 truncate" title={file.name}>
            {file.name}
          </span>
          {(file.kind === "image" ||
            file.kind === "pdf" ||
            file.kind === "office") && (
            <Button
              type="button"
              variant="link"
              size="sm"
              className="h-5 px-0 text-xs"
              aria-label={`预览 ${file.name}`}
              onClick={() => setPreview({ files, index })}
            >
              <Eye className="size-3" />
              预览
            </Button>
          )}
          <Button
            type="button"
            variant="link"
            size="sm"
            className="h-5 px-0 text-xs"
            aria-label={`下载 ${file.name}`}
            onClick={() => {
              void downloadAttachment(file, token).catch(() => {
                toastError("附件下载失败，请重试");
              });
            }}
          >
            <Download className="size-3" />
            下载
          </Button>
        </div>
      ))}
      {preview && (
        <AttachmentPreview
          files={preview.files}
          index={preview.index}
          onChange={(next) => setPreview({ files: preview.files, index: next })}
          onClose={() => setPreview(null)}
        />
      )}
    </div>
  );
}

function isDescriptionField(field: BatchTaskFormField): boolean {
  const fieldType = (field.fieldType ?? "").toLowerCase();
  return ["text", "description", "explanation", "hint"].includes(fieldType);
}

function isDetailField(field: BatchTaskFormField): boolean {
  const fieldType = (field.fieldType ?? "").toLowerCase();
  return fieldType === "fieldlist" || fieldType === "field_list";
}

function dateText(value: string): string {
  const timestamp = new Date(value).getTime();
  return Number.isNaN(timestamp) ? value : formatProductDateTime(timestamp);
}

function fieldDisplayText(field: BatchTaskFormField): string {
  const files = attachmentsOf(field);
  if (files.length > 0) return files.map((file) => file.name).join("、");

  const fieldType = (field.fieldType ?? "").toLowerCase();
  if (fieldType === "date" && typeof field.value === "string") {
    return dateText(field.value);
  }
  if (fieldType === "dateinterval" && field.value) {
    const record =
      typeof field.value === "object" && !Array.isArray(field.value)
        ? (field.value as Record<string, unknown>)
        : null;
    const start = record?.start ?? record?.start_time;
    const end = record?.end ?? record?.end_time;
    if (typeof start === "string" && typeof end === "string") {
      return `${dateText(start)} 至 ${dateText(end)}`;
    }
  }
  if (fieldType === "fieldlist" || fieldType === "field_list") {
    const count =
      field.rowCount ?? (Array.isArray(field.value) ? field.value.length : 0);
    const preview = field.preview.length > 0 ? `：${field.preview.join(" · ")}` : "";
    return `明细 ${count} 行${preview}`;
  }
  if (fieldType === "address" && typeof field.value === "string") {
    return field.value.split("/").join(" / ");
  }
  return fieldValueText(field.value);
}

/// 明细列在单元格里只显示行数与首行预览，展开后由 TaskDetailSection 加载全部行。
function detailFieldOf(
  task: BatchTask,
  field: { id: string | null; name: string },
): BatchTaskFormField | undefined {
  return task.formFields.find(
    (row) =>
      !isDescriptionField(row) &&
      isDetailField(row) &&
      (field.id ? row.id === field.id : row.name === field.name),
  );
}

function formFieldText(
  task: BatchTask,
  field: { id: string | null; name: string },
): string {
  return task.formFields
    .filter(
      (row) =>
        !isDescriptionField(row) &&
        (field.id ? row.id === field.id : row.name === field.name),
    )
    .map(fieldDisplayText)
    .filter(Boolean)
    .join("；");
}

type ColumnFilter = { text: string; value: string };

function uniqueFilters(values: string[]): ColumnFilter[] {
  const seen = new Set<string>();
  const filters: ColumnFilter[] = [];
  for (const value of values) {
    if (!value || seen.has(value)) continue;
    seen.add(value);
    filters.push({ text: value, value });
  }
  return filters.sort((left, right) =>
    left.text.localeCompare(right.text, "zh-CN"),
  );
}

type FormColumn = { id: string | null; name: string; key: string };

/// 表单列以控件 ID 为键；实例数据缺 ID（旧任务）时回退到字段名。
function formColumns(tasks: BatchTask[]): FormColumn[] {
  const seen = new Set<string>();
  const columns: FormColumn[] = [];
  for (const task of tasks) {
    for (const field of task.formFields) {
      if (isDescriptionField(field)) continue;
      const id = field.id ?? null;
      const key = id ? `form:${id}` : `form:${field.name}`;
      if (seen.has(key)) continue;
      seen.add(key);
      columns.push({ id, name: field.name, key });
    }
  }
  return columns;
}

/// 技术定位字段默认不展示，需要时可在字段设置里打开。
const DEFAULT_HIDDEN_COLUMNS = new Set(["base:instanceCode"]);

type ColumnKind = "instance" | "form" | "computed";

const COLUMN_KIND_META: Record<
  ColumnKind,
  { label: string; color: string; hint: string }
> = {
  instance: {
    label: "实例",
    color: "default",
    hint: "审批实例自带的数据",
  },
  form: {
    label: "表单",
    color: "blue",
    hint: "审批表单里的控件值",
  },
  computed: {
    label: "计算",
    color: "purple",
    hint: "按你在字段设置里配置的规则计算得出",
  },
};

/// 表头统一附带类型标记，一眼区分实例数据与个人配置的计算字段。
function columnTitle(label: string, kind: ColumnKind): ReactNode {
  const meta = COLUMN_KIND_META[kind];
  // 标签放到标题下方，避免挤占横向空间把表头文字压成竖排。
  return (
    <span className="inline-flex flex-col items-start gap-0.5 leading-tight">
      <span className="block max-w-full truncate" title={label}>
        {label}
      </span>
      <Tooltip title={meta.hint}>
        <Tag
          color={meta.color}
          variant="filled"
          style={{ marginInlineEnd: 0, fontSize: 10, lineHeight: "16px" }}
        >
          {meta.label}
        </Tag>
      </Tooltip>
    </span>
  );
}

/// 用户保存的列顺序/可见性；未配置时按默认顺序展示，
/// 配置后新增的字段追加在末尾保持可见（技术定位字段除外）。
function applyViewLayout(
  columns: TableColumnsType<BatchTask>,
  view: BatchViewConfig | undefined,
): TableColumnsType<BatchTask> {
  const byKey = new Map(columns.map((column) => [String(column.key), column]));
  const used = new Set<string>();
  const ordered: TableColumnsType<BatchTask> = [];
  for (const entry of view?.columns ?? []) {
    const column = byKey.get(entry.key);
    if (!column || used.has(entry.key)) continue;
    used.add(entry.key);
    if (entry.visible) ordered.push(column);
  }
  for (const column of columns) {
    const key = String(column.key);
    if (used.has(key)) continue;
    used.add(key);
    if (DEFAULT_HIDDEN_COLUMNS.has(key)) continue;
    ordered.push(column);
  }
  return ordered;
}

function taskNodes(task: BatchTask): string[] {
  if (task.currentNodeNames.length > 0) return task.currentNodeNames;
  return task.myNodeName ? [task.myNodeName] : [];
}

function buildTaskColumns(
  tasks: BatchTask[],
  view?: BatchViewConfig,
): TableColumnsType<BatchTask> {
  // 勾选、标题、当前节点、实例状态是固定列，不参与个人视图的排序与隐藏。
  const fixedColumns: TableColumnsType<BatchTask> = [
    {
      title: columnTitle("标题", "instance"),
      dataIndex: "title",
      key: "title",
      width: 220,
      sorter: (left, right) => left.title.localeCompare(right.title, "zh-CN"),
      filters: uniqueFilters(tasks.map((task) => task.title)),
      filterSearch: true,
      onFilter: (value, task) => task.title === String(value),
      render: (_, task) => {
        const operability = taskOperability(task);
        return (
          <div className="max-w-full">
            <span className="block truncate font-medium" title={task.title}>
              {task.title}
            </span>
            {!operability.operable && (
              <span className="block text-xs text-muted-foreground">
                {operability.reason}
              </span>
            )}
          </div>
        );
      },
    },
    {
      title: columnTitle("当前节点", "instance"),
      key: "currentNode",
      width: 240,
      sorter: (left, right) =>
        taskNodes(left)
          .join("、")
          .localeCompare(taskNodes(right).join("、"), "zh-CN"),
      filters: uniqueFilters(tasks.flatMap(taskNodes)),
      filterSearch: true,
      onFilter: (value, task) => taskNodes(task).includes(String(value)),
      render: (_, task) => {
        const nodes = taskNodes(task);
        if (nodes.length === 0) {
          return <span className="text-xs text-muted-foreground">—</span>;
        }
        const ordered = task.myNodeName
          ? [
              task.myNodeName,
              ...nodes.filter((node) => node !== task.myNodeName),
            ]
          : nodes;
        return (
          <div className="flex flex-wrap gap-1">
            {ordered.map((node) => {
              const mine = node === task.myNodeName;
              return (
                <StatusBadge
                  key={node}
                  tone={mine ? "info" : "neutral"}
                  title={node}
                >
                  {node}
                  {mine ? "（我的节点）" : ""}
                </StatusBadge>
              );
            })}
          </div>
        );
      },
    },
    {
      title: columnTitle("实例状态", "instance"),
      key: "status",
      width: 120,
      sorter: (left, right) =>
        taskStatusLabel(left).localeCompare(taskStatusLabel(right), "zh-CN"),
      filters: uniqueFilters(tasks.map(taskStatusLabel)),
      onFilter: (value, task) => taskStatusLabel(task) === String(value),
      render: (_, task) => {
        const label = taskStatusLabel(task);
        return <StatusBadge tone={statusTone(label)}>{label}</StatusBadge>;
      },
    },
  ];

  const baseColumns: TableColumnsType<BatchTask> = [
    {
      title: columnTitle("审批定义", "instance"),
      key: "definition",
      width: 220,
      sorter: (left, right) =>
        left.definitionName.localeCompare(right.definitionName, "zh-CN"),
      filters: uniqueFilters(tasks.map((task) => task.definitionName)),
      filterSearch: true,
      onFilter: (value, task) => task.definitionName === String(value),
      render: (_, task) => (
        <div className="max-w-full">
          <span className="block truncate" title={task.definitionName}>
            {task.definitionName}
          </span>
          <span
            className="block truncate text-xs text-muted-foreground"
            title={task.definitionCode || "—"}
          >
            {task.definitionCode || "—"}
          </span>
        </div>
      ),
    },
    {
      title: columnTitle("实例编号", "instance"),
      dataIndex: "instanceCode",
      key: "instanceCode",
      width: 300,
      sorter: (left, right) =>
        left.instanceCode.localeCompare(right.instanceCode),
      filters: uniqueFilters(tasks.map((task) => task.instanceCode)),
      filterSearch: true,
      onFilter: (value, task) => task.instanceCode === String(value),
      render: (_, task) => (
        <span className="text-xs" title={task.instanceCode}>
          {task.instanceCode}
        </span>
      ),
    },
    {
      title: columnTitle("发起人", "instance"),
      key: "initiator",
      width: 120,
      sorter: (left, right) =>
        (left.initiatorName ?? "").localeCompare(
          right.initiatorName ?? "",
          "zh-CN",
        ),
      filters: uniqueFilters(tasks.map((task) => task.initiatorName ?? "—")),
      onFilter: (value, task) => (task.initiatorName ?? "—") === String(value),
      render: (_, task) => (
        <span title={task.initiatorName ?? "—"}>
          {task.initiatorName ?? "—"}
        </span>
      ),
    },
    {
      title: columnTitle("摘要", "instance"),
      key: "summary",
      width: 280,
      filters: uniqueFilters(
        tasks.map((task) => task.summaries.join("；") || "—"),
      ),
      filterSearch: true,
      onFilter: (value, task) =>
        (task.summaries.join("；") || "—") === String(value),
      render: (_, task) => {
        const summary = task.summaries.join("；") || "—";
        return (
          <span className="block max-w-full truncate text-xs" title={summary}>
            {summary}
          </span>
        );
      },
    },
    {
      title: columnTitle("提交时间", "instance"),
      key: "createdAt",
      width: 180,
      sorter: (left, right) => (left.createdAt ?? 0) - (right.createdAt ?? 0),
      defaultSortOrder: "ascend",
      filters: uniqueFilters(tasks.map((task) => dateKey(task.createdAt))),
      onFilter: (value, task) => dateKey(task.createdAt) === String(value),
      render: (_, task) => {
        const waiting = waitingText(task.createdAt);
        return (
          <div className="leading-tight">
            <span className="block tabular-nums" title={timeText(task.createdAt)}>
              {timeText(task.createdAt)}
            </span>
            {waiting && (
              <span className={`block text-xs ${waiting.className}`}>
                {waiting.text}
              </span>
            )}
          </div>
        );
      },
    },
    {
      title: columnTitle("可批量处理", "instance"),
      key: "apiOperate",
      width: 130,
      filters: [
        { text: "可批量处理", value: "supported" },
        { text: "需在飞书处理", value: "unsupported" },
      ],
      onFilter: (value, task) =>
        value === "supported"
          ? task.supportApiOperate
          : !task.supportApiOperate,
      render: (_, task) => (
        <StatusBadge tone={task.supportApiOperate ? "positive" : "warning"}>
          {task.supportApiOperate ? "可批量处理" : "需在飞书处理"}
        </StatusBadge>
      ),
    },
  ];

  const columns: TableColumnsType<BatchTask> = baseColumns.map((column) => ({
    ...column,
    key: `base:${String(column.key)}`,
  }));

  for (const field of formColumns(tasks)) {
    const values = tasks.map((task) => formFieldText(task, field) || "—");
    columns.push({
      title: columnTitle(field.name, "form"),
      key: field.key,
      width: 180,
      filters: uniqueFilters(values),
      filterSearch: true,
      onFilter: (value, task) =>
        (formFieldText(task, field) || "—") === String(value),
      render: (_, task) => {
        const detail = detailFieldOf(task, field);
        if (detail) {
          const count =
            detail.rowCount ??
            (Array.isArray(detail.value) ? detail.value.length : 0);
          const preview = detail.preview.join(" · ");
          return (
            <div className="flex min-w-0 flex-col gap-0.5">
              <span className="text-xs" data-slot="detail-count">
                <b className="font-semibold">{count}</b> 行
              </span>
              <span
                className="truncate text-[11px] text-muted-foreground"
                title={preview ? `首行：${preview}` : undefined}
              >
                首行：{preview || "—"}
              </span>
            </div>
          );
        }
        const files = task.formFields
          .filter(
            (row) =>
              !isDescriptionField(row) &&
              (field.id ? row.id === field.id : row.name === field.name),
          )
          .flatMap(attachmentsOf);
        if (files.length > 0) {
          return <AttachmentCell files={files} />;
        }
        const text = formFieldText(task, field) || "—";
        return (
          <span className="block max-w-full truncate text-xs" title={text}>
            {text}
          </span>
        );
      },
    });
  }

  for (const field of computedFieldNames(tasks)) {
    columns.push({
      title: columnTitle(field.name, "computed"),
      key: field.key,
      width: 160,
      align: "right",
      sorter: (left, right) =>
        computedFieldValue(left, field.key) -
        computedFieldValue(right, field.key),
      filters: uniqueFilters(
        tasks.map((task) => computedFieldText(task, field.key) || "—"),
      ),
      filterSearch: true,
      onFilter: (value, task) =>
        (computedFieldText(task, field.key) || "—") === String(value),
      render: (_, task) => {
        const text = computedFieldText(task, field.key) || "—";
        return (
          <span
            className="block tabular-nums"
            title={text}
            data-slot="computed-field"
          >
            {text}
          </span>
        );
      },
    });
  }

  return [...fixedColumns, ...applyViewLayout(columns, view)];
}

function computedFieldNames(
  tasks: BatchTask[],
): Array<{ key: string; name: string }> {
  const seen = new Set<string>();
  const fields: Array<{ key: string; name: string }> = [];
  for (const task of tasks) {
    for (const field of task.computedFields) {
      if (seen.has(field.key)) continue;
      seen.add(field.key);
      fields.push({ key: field.key, name: field.name });
    }
  }
  return fields;
}

function computedFieldValue(task: BatchTask, key: string): number {
  return task.computedFields.find((field) => field.key === key)?.value ?? -1;
}

function computedFieldText(task: BatchTask, key: string): string {
  return (
    task.computedFields.find((field) => field.key === key)?.display?.trim() ??
    ""
  );
}

type ResizableHeaderCellProps = HTMLAttributes<HTMLTableCellElement> & {
  width?: number;
  onResize?: (event: SyntheticEvent, data: ResizeCallbackData) => void;
};

function ResizableHeaderCell({
  width,
  onResize,
  ...rest
}: ResizableHeaderCellProps) {
  if (width === undefined) return <th {...rest} />;
  return (
    <Resizable
      width={width}
      height={0}
      onResize={onResize}
      draggableOpts={{ enableUserSelectHack: false }}
      handle={
        <span
          className="absolute top-0 right-0 z-10 h-full w-2 cursor-col-resize select-none"
          title="拖动调整列宽"
          onClick={(event) => event.stopPropagation()}
        />
      }
    >
      <th {...rest} style={{ ...rest.style, position: "relative" }} />
    </Resizable>
  );
}

const TABLE_COMPONENTS = { header: { cell: ResizableHeaderCell } };

const DETAIL_PAGE_SIZE = 50;
const DETAIL_ROW_HEIGHT = 34;
/// 明细只有一两行时不要撑出固定高度，最多 300px。
const DETAIL_MAX_HEIGHT = 300;
const DETAIL_MIN_HEIGHT = 72;

/// 明细列顺序/可见性来自字段设置里的 `detail:<明细控件>:<子控件>` 配置；
/// 没有配置时按控件目录顺序全部展示。
function detailVisibleColumns(
  view: BatchViewConfig | undefined,
  fieldId: string,
  columns: BatchTaskDetailColumn[],
): BatchTaskDetailColumn[] {
  const prefix = `detail:${fieldId}:`;
  const configured = (view?.columns ?? []).filter((column) =>
    column.key.startsWith(prefix),
  );
  if (configured.length === 0) return columns;
  const byId = new Map(columns.map((column) => [column.id, column]));
  return configured
    .filter((column) => column.visible)
    .map((column) => byId.get(column.key.slice(prefix.length)))
    .filter((column): column is BatchTaskDetailColumn => column !== undefined);
}

/// 单条任务的明细控件：按需分页加载 + 固定行高虚拟滚动，接近底部自动续读。
function TaskDetailSection({
  task,
  field,
  view,
  accessToken,
}: {
  task: BatchTask;
  field: BatchTaskFormField;
  view?: BatchViewConfig;
  accessToken: string | undefined;
}) {
  const fieldId = field.id ?? "";
  const [rows, setRows] = useState<BatchTaskDetailCell[][]>([]);
  const [columns, setColumns] = useState<BatchTaskDetailColumn[]>([]);
  const [total, setTotal] = useState(field.rowCount ?? 0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [scrollTop, setScrollTop] = useState(0);
  const loadingRef = useRef(false);
  const loadedRef = useRef(0);
  const doneRef = useRef(false);

  const loadMore = useCallback(async () => {
    if (!fieldId || loadingRef.current || doneRef.current) return;
    loadingRef.current = true;
    setLoading(true);
    try {
      const page = await fetchBatchTaskRows(
        task.taskId,
        task.instanceCode,
        fieldId,
        { offset: loadedRef.current, limit: DETAIL_PAGE_SIZE },
        accessToken,
      );
      setColumns((current) => (current.length > 0 ? current : page.columns));
      setTotal(page.unfilteredTotal);
      loadedRef.current += page.rows.length;
      if (page.rows.length > 0) {
        setRows((current) => [...current, ...page.rows]);
      }
      if (page.rows.length === 0 || loadedRef.current >= page.unfilteredTotal) {
        doneRef.current = true;
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "读取明细失败");
      doneRef.current = true;
    } finally {
      loadingRef.current = false;
      setLoading(false);
    }
  }, [accessToken, fieldId, task.instanceCode, task.taskId]);

  useEffect(() => {
    void loadMore();
  }, [loadMore]);

  const visibleColumns = useMemo(
    () => detailVisibleColumns(view, fieldId, columns),
    [columns, fieldId, view],
  );
  const viewportHeight =
    rows.length === 0
      ? DETAIL_MIN_HEIGHT
      : Math.min(
          Math.max(rows.length * DETAIL_ROW_HEIGHT, DETAIL_ROW_HEIGHT),
          DETAIL_MAX_HEIGHT,
        );
  const start = Math.max(0, Math.floor(scrollTop / DETAIL_ROW_HEIGHT) - 2);
  const end = Math.min(
    rows.length,
    start + Math.ceil(viewportHeight / DETAIL_ROW_HEIGHT) + 4,
  );
  const gridTemplate = `44px repeat(${Math.max(visibleColumns.length, 1)}, minmax(0, 1fr))`;
  const minWidth = 44 + Math.max(visibleColumns.length, 1) * 120;

  return (
    <div data-slot="task-detail">
      <div className="mb-2 flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="text-[13px] font-semibold">明细 · {field.name}</span>
        <span className="text-xs text-muted-foreground">
          共 {total} 行 · 已加载 {rows.length} 行
        </span>
        {loading && (
          <span className="text-xs text-muted-foreground">加载中…</span>
        )}
        {error && <span className="text-xs text-destructive">{error}</span>}
      </div>
      {!error && visibleColumns.length === 0 && !loading ? (
        <p className="text-xs text-muted-foreground">
          明细列都已隐藏，可在「字段设置」里勾选要显示的明细列。
        </p>
      ) : (
        !error && (
          <div className="overflow-hidden rounded-sm border bg-card">
            <div
              className="grid border-b bg-muted/60 text-xs font-medium text-muted-foreground"
              style={{ gridTemplateColumns: gridTemplate, minWidth }}
            >
              <div className="px-2.5 py-2 text-center">#</div>
              {visibleColumns.map((column) => (
                <div
                  key={column.id}
                  className="truncate border-l px-2.5 py-2"
                  title={column.name}
                >
                  {column.name}
                </div>
              ))}
            </div>
            <div
              className="overflow-auto"
              style={{ height: viewportHeight }}
              onScroll={(event) => {
                const element = event.currentTarget;
                setScrollTop(element.scrollTop);
                if (
                  element.scrollHeight -
                    element.scrollTop -
                    element.clientHeight <
                  120
                ) {
                  void loadMore();
                }
              }}
            >
              <div
                style={{
                  height: rows.length * DETAIL_ROW_HEIGHT,
                  minWidth,
                  position: "relative",
                }}
              >
                <div
                  style={{
                    position: "absolute",
                    top: start * DETAIL_ROW_HEIGHT,
                    left: 0,
                    right: 0,
                  }}
                >
                  {rows.slice(start, end).map((row, index) => (
                    <div
                      key={start + index}
                      className="grid items-center border-b text-[13px] last:border-b-0 hover:bg-muted/40"
                      style={{
                        height: DETAIL_ROW_HEIGHT,
                        gridTemplateColumns: gridTemplate,
                      }}
                    >
                      <div className="truncate px-2.5 text-center text-xs tabular-nums text-muted-foreground">
                        {start + index + 1}
                      </div>
                      {visibleColumns.map((column) => {
                        const cell = row.find((item) => item.id === column.id);
                        const text = fieldValueText(cell?.value);
                        return (
                          <div
                            key={column.id}
                            className="truncate border-l px-2.5"
                            title={text}
                          >
                            {text}
                          </div>
                        );
                      })}
                    </div>
                  ))}
                </div>
              </div>
            </div>
          </div>
        )
      )}
    </div>
  );
}

function TaskDetailPanels({
  task,
  view,
  accessToken,
}: {
  task: BatchTask;
  view?: BatchViewConfig;
  accessToken: string | undefined;
}) {
  const details = task.formFields.filter(
    (field) => isDetailField(field) && field.id,
  );
  if (details.length === 0) {
    return <p className="text-xs text-muted-foreground">暂无明细</p>;
  }
  return (
    <div className="space-y-4 px-3.5 py-3">
      {details.map((field) => (
        <TaskDetailSection
          key={field.id}
          task={task}
          field={field}
          view={view}
          accessToken={accessToken}
        />
      ))}
    </div>
  );
}

function DefinitionTable({
  tasks,
  definitionKey,
  view,
  accessToken,
  selected,
  onSelect,
  onFilteredTasksChange,
}: {
  tasks: BatchTask[];
  definitionKey: string;
  view?: BatchViewConfig;
  accessToken: string | undefined;
  selected: SelectedMap;
  onSelect: (tableTasks: BatchTask[], selectedTasks: BatchTask[]) => void;
  onFilteredTasksChange: (definitionKey: string, taskIds: string[]) => void;
}) {
  const [expandedKeys, setExpandedKeys] = useState<string[]>([]);
  const baseColumns = useMemo(
    () => buildTaskColumns(tasks, view),
    [tasks, view],
  );
  const [columnWidths, setColumnWidths] = useState<Record<string, number>>({});
  const columns = useMemo(
    () =>
      baseColumns.map((column, index) => {
        const key = String(column.key ?? index);
        const width =
          columnWidths[key] ??
          (typeof column.width === "number" ? column.width : undefined);
        return {
          ...column,
          width,
          onHeaderCell: () => ({
            width,
            onResize: (_event: SyntheticEvent, data: ResizeCallbackData) => {
              setColumnWidths((previous) => ({
                ...previous,
                [key]: Math.max(100, Math.round(data.size.width)),
              }));
            },
          }),
        };
      }),
    [baseColumns, columnWidths],
  );
  const selectedRowKeys = tasks
    .filter((task) => selected[task.taskId])
    .map((task) => task.taskId);

  const expandableTasks = tasks.filter((task) =>
    task.formFields.some((field) => isDetailField(field) && field.id),
  );
  const allExpanded =
    expandableTasks.length > 0 &&
    expandableTasks.every((task) => expandedKeys.includes(task.taskId));
  const columnsWithExpand = useMemo(
    () =>
      expandableTasks.length > 0
        ? ([Table.EXPAND_COLUMN, ...columns] as TableColumnsType<BatchTask>)
        : columns,
    [columns, expandableTasks.length],
  );

  return (
    <ConfigProvider locale={zhCN}>
      <style>{`
        .batch-expanded-row > td { background: var(--muted) !important; padding: 0 !important; }
        .batch-expanded-row:hover > td { background: var(--muted) !important; }
      `}</style>
      {expandableTasks.length > 0 && (
        <div className="flex justify-end">
          <Button
            type="button"
            variant="ghost"
            size="sm"
            onClick={() =>
              setExpandedKeys(
                allExpanded ? [] : expandableTasks.map((task) => task.taskId),
              )
            }
          >
            {allExpanded ? "全部收起" : "全部展开"}
          </Button>
        </div>
      )}
      <Table<BatchTask>
        rowKey="taskId"
        size="small"
        bordered
        tableLayout="fixed"
        columns={columnsWithExpand}
        components={TABLE_COMPONENTS}
        dataSource={tasks}
        onChange={(_pagination, _filters, _sorter, extra) =>
          onFilteredTasksChange(
            definitionKey,
            extra.currentDataSource.map((task) => task.taskId),
          )
        }
        expandable={{
          columnWidth: 44,
          expandedRowKeys: expandedKeys,
          expandedRowClassName: () => "batch-expanded-row",
          onExpandedRowsChange: (keys) =>
            setExpandedKeys(keys.map((key) => String(key))),
          rowExpandable: (task) =>
            task.formFields.some((field) => isDetailField(field) && field.id),
          expandedRowRender: (task) => (
            <TaskDetailPanels
              task={task}
              view={view}
              accessToken={accessToken}
            />
          ),
        }}
        scroll={{ x: "max-content" }}
        pagination={{
          pageSize: 20,
          pageSizeOptions: [20, 50, 100],
          showSizeChanger: true,
          hideOnSinglePage: true,
          showTotal: (total) => `共 ${total} 条`,
        }}
        rowSelection={{
          selectedRowKeys,
          preserveSelectedRowKeys: true,
          getCheckboxProps: (task) => ({
            disabled: !taskOperability(task).operable,
            "aria-label": `选择 ${task.title}`,
          }),
          onChange: (keys) => {
            const selectedKeys = new Set(keys.map(String));
            onSelect(
              tasks,
              tasks.filter((task) => selectedKeys.has(task.taskId)),
            );
          },
        }}
        locale={{ emptyText: "没有符合筛选条件的待办" }}
      />
    </ConfigProvider>
  );
}

/// 批量审批页：独立专注页（根层 RequireAuth，不套 AppLayout）。
///
/// 数据流：拉本人待办 → 表格展示与逐列筛选 → 勾选可操作任务 → 在确认对话框里选动作
/// → 创建 job → 轮询逐条结果。表单字段与当前节点直接展开成表格列，不再需要逐条打开详情。
export default function BatchApprovalPage() {
  const session = useSessionCredentials();
  const token = session.token;
  const controller = useSessionController();
  const navigate = useNavigate();
  const toast = useToast();

  const [tasks, setTasks] = useState<BatchTask[]>([]);
  const [views, setViews] = useState<Record<string, BatchViewConfig>>({});
  const [settingsCode, setSettingsCode] = useState<string | null>(null);
  const [truncated, setTruncated] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState("");
  const [reauthRequired, setReauthRequired] = useState(false);
  const [scopeError, setScopeError] = useState(false);
  const [username, setUsername] = useState<string | null>(null);
  const [loggingOut, setLoggingOut] = useState(false);
  const [authorizing, setAuthorizing] = useState(false);
  const [reauthOpen, setReauthOpen] = useState(false);
  const [reauthPassword, setReauthPassword] = useState("");
  const [reauthError, setReauthError] = useState("");

  const [definitionGroup, setDefinitionGroup] = useState("all");
  const [keyword, setKeyword] = useState("");
  const [tableFilteredIds, setTableFilteredIds] = useState<
    Record<string, string[]>
  >({});
  const [tableVersion, setTableVersion] = useState(0);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const [selected, setSelected] = useState<SelectedMap>({});
  const [action, setAction] = useState<BatchApprovalAction | null>(null);
  const [actionError, setActionError] = useState("");
  const [submittingAction, setSubmittingAction] = useState(false);
  const [job, setJob] = useState<{ id: string; label: string } | null>(null);
  const [lastRequest, setLastRequest] = useState<LastJobRequest | null>(null);
  const [retrying, setRetrying] = useState(false);

  const loadTasks = useCallback(
    async (signal?: AbortSignal) => {
      setLoading(true);
      setLoadError("");
      setReauthRequired(false);
      setScopeError(false);
      try {
        const result = await fetchBatchTasks(token, signal);
        if (signal?.aborted) return;
        setTasks(result.tasks);
        setTruncated(result.truncated);
        setViews(result.views);
        // 数据刷新后清空列筛选状态，避免筛选条件与新数据不一致。
        setTableFilteredIds({});
        setTableVersion((version) => version + 1);
        // 刷新后只保留仍然存在且可操作的选择，避免把已处理任务再次提交。
        setSelected((prev) => {
          const next: SelectedMap = {};
          for (const task of result.tasks) {
            if (prev[task.taskId] && taskOperability(task).operable) {
              next[task.taskId] = task;
            }
          }
          return next;
        });
      } catch (cause) {
        if (signal?.aborted) return;
        setTasks([]);
        setTruncated(false);
        if (isFeishuReauthError(cause)) {
          setReauthRequired(true);
        } else if (isFeishuScopeError(cause)) {
          setScopeError(true);
        } else {
          setLoadError(describeBatchError(cause));
        }
      } finally {
        if (!signal?.aborted) setLoading(false);
      }
    },
    [token],
  );

  useEffect(() => {
    const abort = new AbortController();
    void loadTasks(abort.signal);
    void fetchCurrentUsername(token, abort.signal)
      .then((name) => {
        if (!abort.signal.aborted) setUsername(name);
      })
      .catch(() => {
        // 当前用户名只是页眉信息，取不到就显示占位，不阻塞任务列表。
      });
    return () => abort.abort();
  }, [loadTasks, token]);

  const groups = useMemo(() => groupBatchTasks(tasks), [tasks]);
  const normalizedKeyword = keyword.trim().toLowerCase();
  const filteredGroups = useMemo(() => {
    if (!normalizedKeyword) return groups;
    return groups
      .map((group) => ({
        ...group,
        tasks: group.tasks.filter((task) =>
          [
            task.title,
            task.instanceCode,
            task.initiatorName ?? "",
            task.definitionName,
            task.summaries.join(" "),
            task.currentNodeNames.join(" "),
          ]
            .join("\n")
            .toLowerCase()
            .includes(normalizedKeyword),
        ),
      }))
      .filter((group) => group.tasks.length > 0);
  }, [groups, normalizedKeyword]);
  const visibleGroups = useMemo(
    () =>
      definitionGroup === "all"
        ? filteredGroups
        : filteredGroups.filter((group) => group.key === definitionGroup),
    [definitionGroup, filteredGroups],
  );
  const visibleTaskCount = useMemo(
    () => visibleGroups.reduce((total, group) => total + group.tasks.length, 0),
    [visibleGroups],
  );
  const tableFilteredSets = useMemo(() => {
    const sets = new Map<string, Set<string>>();
    for (const [key, ids] of Object.entries(tableFilteredIds)) {
      sets.set(key, new Set(ids));
    }
    return sets;
  }, [tableFilteredIds]);
  const taskVisibleInTable = useCallback(
    (groupKey: string, taskId: string) => {
      const allowed = tableFilteredSets.get(groupKey);
      return allowed === undefined || allowed.has(taskId);
    },
    [tableFilteredSets],
  );
  const operableVisibleTasks = useMemo(
    () =>
      visibleGroups
        .flatMap((group) =>
          group.tasks.filter((task) => taskVisibleInTable(group.key, task.taskId)),
        )
        .filter((task) => taskOperability(task).operable),
    [visibleGroups, taskVisibleInTable],
  );
  const filteredTotals = useMemo(
    () =>
      new Map(
        groups.map((group) => [
          group.key,
          filteredGroups.find((candidate) => candidate.key === group.key)
            ?.tasks.length ?? 0,
        ]),
      ),
    [filteredGroups, groups],
  );
  const filteredTotalCount = useMemo(
    () => filteredGroups.reduce((total, group) => total + group.tasks.length, 0),
    [filteredGroups],
  );
  const visibleTaskIds = useMemo(
    () =>
      new Set(
        visibleGroups.flatMap((group) =>
          group.tasks
            .filter((task) => taskVisibleInTable(group.key, task.taskId))
            .map((task) => task.taskId),
        ),
      ),
    [visibleGroups, taskVisibleInTable],
  );
  const selectedTasks = useMemo(
    () => Object.values(selected).filter((task) => visibleTaskIds.has(task.taskId)),
    [selected, visibleTaskIds],
  );
  const hiddenSelectedCount = Object.keys(selected).length - selectedTasks.length;

  const resetFilters = () => {
    setDefinitionGroup("all");
    setKeyword("");
  };

  const selectAllOperable = () => {
    setSelected((previous) => {
      const next = { ...previous };
      for (const task of operableVisibleTasks) next[task.taskId] = task;
      return next;
    });
  };

  const replaceTableSelection = (
    tableTasks: BatchTask[],
    nextSelection: BatchTask[],
  ) => {
    setSelected((prev) => {
      const next = { ...prev };
      for (const task of tableTasks) delete next[task.taskId];
      for (const task of nextSelection) {
        if (taskOperability(task).operable) next[task.taskId] = task;
      }
      return next;
    });
  };

  const openAction = (nextAction: BatchApprovalAction) => {
    if (selectedTasks.length === 0) {
      toast.info("请先勾选可操作的任务");
      return;
    }
    setActionError("");
    setAction(nextAction);
  };

  const jobParams = (params: BatchActionParams) => ({
    comment: params.comment,
    transferOpenId: params.transferUser?.openId,
    addSignOpenIds: params.addSignUsers?.map((user) => user.openId),
    addSignType: params.addSignType,
    approvalMethod: params.approvalMethod,
    rollbackMode: params.rollbackMode,
  });

  const confirmAction = async (params: BatchActionParams) => {
    if (!action || selectedTasks.length === 0 || submittingAction) return;
    const items: BatchJobItem[] = selectedTasks.map((task) => ({
      taskId: task.taskId,
      instanceCode: task.instanceCode,
      title: task.title,
      status: "pending",
      resultCode: null,
      message: null,
      beforeStatus: null,
      afterStatus: null,
    }));
    setSubmittingAction(true);
    setActionError("");
    try {
      const jobId = await createBatchJob(
        {
          action,
          items,
          idempotencyKey: newIdempotencyKey(),
          ...jobParams(params),
        },
        token,
      );
      setLastRequest({ action, params, items });
      setAction(null);
      setSelected({});
      setJob({ id: jobId, label: BATCH_ACTION_LABELS[action] });
      toast.success("批量任务已创建，正在后台执行");
    } catch (cause) {
      if (isFeishuReauthError(cause)) {
        setAction(null);
        setReauthRequired(true);
        setLoadError(describeBatchError(cause));
      } else {
        setActionError(describeBatchError(cause));
      }
    } finally {
      setSubmittingAction(false);
    }
  };

  const retryFailed = async (failedItems: BatchJobItem[]) => {
    if (!lastRequest || retrying) return;
    setRetrying(true);
    try {
      const jobId = await createBatchJob(
        {
          action: lastRequest.action,
          items: failedItems,
          idempotencyKey: newIdempotencyKey(),
          ...jobParams(lastRequest.params),
        },
        token,
      );
      setJob({
        id: jobId,
        label: `重试${BATCH_ACTION_LABELS[lastRequest.action]}`,
      });
    } catch (cause) {
      toast.error(describeBatchError(cause));
    } finally {
      setRetrying(false);
    }
  };

  const closeJob = () => {
    setJob(null);
    void loadTasks();
  };

  const reauthorize = () => {
    if (authorizing) return;
    setReauthPassword("");
    setReauthError("");
    setReauthOpen(true);
  };

  const submitReauthorize = async () => {
    if (authorizing) return;
    if (!reauthPassword) {
      setReauthError("请输入当前账号密码");
      return;
    }
    setAuthorizing(true);
    setReauthError("");
    try {
      const url = await requestFeishuAuthorizationUrl(token, reauthPassword);
      window.location.assign(url);
    } catch (cause) {
      setReauthError(describeBatchError(cause));
      setAuthorizing(false);
    }
  };

  const logout = async () => {
    if (loggingOut) return;
    setLoggingOut(true);
    try {
      await controller.endSession();
      navigate("/login", { replace: true });
    } finally {
      setLoggingOut(false);
    }
  };

  return (
    <div className="flex min-h-svh flex-col bg-background text-foreground">
      <header className="sticky top-0 z-10 flex flex-wrap items-center justify-between gap-3 border-b border-border bg-background/95 px-6 py-3 backdrop-blur">
        <div className="flex items-center gap-2 text-sm font-semibold">
          <span className="flex size-8 items-center justify-center rounded-lg bg-primary text-sm font-bold text-primary-foreground">
            Y
          </span>
          YANG System
          <span className="font-normal text-muted-foreground">· 批量审批</span>
        </div>
        <div className="flex flex-wrap items-center gap-3 text-sm">
          <span className="text-muted-foreground">
            当前用户：{username ?? "—"}
          </span>
          <Button variant="ghost" size="sm" asChild>
            <Link to="/">返回系统</Link>
          </Button>
          <Button
            variant="outline"
            size="sm"
            disabled={loggingOut}
            onClick={() => void logout()}
          >
            {loggingOut ? "退出中…" : "退出登录"}
          </Button>
        </div>
      </header>

      <main className="mx-auto flex w-full max-w-[1600px] flex-1 flex-col gap-4 p-6">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <h1 className="text-xl font-semibold">批量审批</h1>
            <p className="text-sm text-muted-foreground">
              处理你自己的飞书待办，默认最早提交的排在前面。写操作由后台逐条执行，不自动重试。
            </p>
          </div>
          <Button
            variant="outline"
            size="sm"
            disabled={loading}
            onClick={() => void loadTasks()}
          >
            <RefreshCw className="size-4" aria-hidden="true" />
            {loading ? "刷新中…" : "刷新"}
          </Button>
        </div>

        {truncated && (
          <p className="rounded-md border border-tone-warning/40 bg-tone-warning/10 px-3 py-2 text-sm">
            待办超过 1000 条，仅显示前 1000 条。请按审批定义筛选或分批处理。
          </p>
        )}
        {reauthRequired && (
          <div
            role="alert"
            className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-tone-warning/40 bg-tone-warning/10 px-3 py-2 text-sm"
          >
            <span>
              飞书授权已过期或尚未绑定，无法读取待办。请重新授权飞书账号。
            </span>
            <Button
              size="sm"
              disabled={authorizing}
              onClick={() => void reauthorize()}
            >
              {authorizing ? "正在跳转…" : "重新授权飞书"}
            </Button>
          </div>
        )}
        {scopeError && (
          <p
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            飞书应用未开通审批/通讯录权限，请联系管理员在开放平台开通并发布版本。
          </p>
        )}
        {loadError && (
          <div
            role="alert"
            className="flex flex-wrap items-center justify-between gap-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            <span>{loadError}</span>
            <Button
              variant="outline"
              size="sm"
              disabled={loading}
              onClick={() => void loadTasks()}
            >
              重试
            </Button>
          </div>
        )}

        <div className="flex flex-1 flex-col gap-4 md:flex-row">
          <aside
            aria-label="审批定义"
            className={
              sidebarCollapsed
                ? "w-full shrink-0 md:w-10"
                : "w-full shrink-0 space-y-1 md:w-60"
            }
          >
            {sidebarCollapsed ? (
              <Button
                variant="outline"
                size="sm"
                className="w-full px-0 md:w-10"
                aria-label="展开审批定义侧栏"
                title="展开审批定义侧栏"
                onClick={() => setSidebarCollapsed(false)}
              >
                <ChevronRight className="size-4" />
              </Button>
            ) : (
              <>
                <div className="flex items-center justify-between px-2">
                  <p className="text-xs font-medium text-muted-foreground">
                    按审批定义
                  </p>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-6 px-1.5"
                    aria-label="收起审批定义侧栏"
                    title="收起审批定义侧栏"
                    onClick={() => setSidebarCollapsed(true)}
                  >
                    <ChevronLeft className="size-4" />
                  </Button>
                </div>
                <button
                  type="button"
                  aria-pressed={definitionGroup === "all"}
                  className={
                    definitionGroup === "all"
                      ? "flex w-full items-center justify-between rounded-md bg-primary/10 px-3 py-2 text-left text-sm text-primary"
                      : "flex w-full items-center justify-between rounded-md px-3 py-2 text-left text-sm text-muted-foreground hover:bg-muted"
                  }
                  onClick={() => setDefinitionGroup("all")}
                >
                  <span>全部待办</span>
                  <span className="tabular-nums">{filteredTotalCount}</span>
                </button>
                {groups.map((group) => (
                  <button
                    key={group.key}
                    type="button"
                    aria-pressed={definitionGroup === group.key}
                    className={
                      definitionGroup === group.key
                        ? "flex w-full items-center justify-between gap-2 rounded-md bg-primary/10 px-3 py-2 text-left text-sm text-primary"
                        : "flex w-full items-center justify-between gap-2 rounded-md px-3 py-2 text-left text-sm text-muted-foreground hover:bg-muted"
                    }
                    onClick={() => setDefinitionGroup(group.key)}
                  >
                    <span className="min-w-0">
                      <span className="block truncate">{group.name}</span>
                      {group.code && (
                        <span className="block truncate text-xs opacity-70">
                          {group.code}
                        </span>
                      )}
                    </span>
                    <span className="tabular-nums">
                      {filteredTotals.get(group.key) ?? 0}
                    </span>
                  </button>
                ))}
              </>
            )}
          </aside>

          <section className="min-w-0 flex-1 space-y-3">
            <div className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-card px-3 py-2 md:sticky md:top-14 md:z-20 md:bg-card/95 md:backdrop-blur">
              <div className="relative w-full sm:w-64">
                <Input
                  value={keyword}
                  onChange={(event) => setKeyword(event.target.value)}
                  placeholder="搜索标题、单号、发起人、摘要"
                  aria-label="搜索待办"
                  className="h-8 bg-background pr-7 text-sm"
                />
                {keyword && (
                  <button
                    type="button"
                    aria-label="清空搜索"
                    className="absolute top-1/2 right-2 -translate-y-1/2 text-muted-foreground transition-colors hover:text-foreground"
                    onClick={() => setKeyword("")}
                  >
                    ×
                  </button>
                )}
              </div>
              <span className="text-sm text-muted-foreground">
                已选 {selectedTasks.length} 条 · 可处理{" "}
                {operableVisibleTasks.length} 条 · 共 {visibleTaskCount} 条
                {hiddenSelectedCount > 0 && (
                  <span className="ml-1 text-tone-warning">
                    （另有 {hiddenSelectedCount} 条已选项不在当前筛选内）
                  </span>
                )}
              </span>
              <Button
                variant="outline"
                size="sm"
                disabled={
                  operableVisibleTasks.length === 0 ||
                  operableVisibleTasks.every(
                    (task) => selected[task.taskId],
                  )
                }
                onClick={selectAllOperable}
              >
                全选可处理
              </Button>
              <Button
                variant="ghost"
                size="sm"
                disabled={selectedTasks.length === 0}
                onClick={() => setSelected({})}
              >
                清空选择
              </Button>
              <div className="ml-auto flex flex-wrap gap-2">
                <Button
                  size="sm"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("pass")}
                >
                  同意
                </Button>
                <Button
                  size="sm"
                  variant="destructive"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("refuse")}
                >
                  拒绝
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("forward")}
                >
                  转交
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("rollback")}
                >
                  退回
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={selectedTasks.length === 0}
                  onClick={() => openAction("add_sign")}
                >
                  加签
                </Button>
              </div>
            </div>

            {loading && tasks.length === 0 ? (
              <div className="space-y-2" aria-label="待办加载中">
                <Skeleton className="h-10 w-full" />
                <Skeleton className="h-10 w-full" />
                <Skeleton className="h-10 w-full" />
              </div>
            ) : tasks.length === 0 ? (
              reauthRequired || loadError || scopeError ? null : (
                <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border py-16 text-center">
                  <Inbox className="size-10 text-muted-foreground" />
                  <p className="font-medium">暂无待办</p>
                  <p className="text-sm text-muted-foreground">
                    当前飞书账号没有待你处理的审批；刷新可重新拉取。
                  </p>
                </div>
              )
            ) : visibleGroups.length === 0 ? (
              <div className="flex flex-col items-center gap-3 rounded-lg border border-dashed border-border py-16 text-center">
                <p className="font-medium">
                  {keyword.trim() ? "没有匹配的待办" : "该审批定义暂无待办"}
                </p>
                {keyword.trim() ? (
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setKeyword("")}
                  >
                    清空搜索
                  </Button>
                ) : (
                  <Button variant="outline" size="sm" onClick={resetFilters}>
                    查看全部待办
                  </Button>
                )}
              </div>
            ) : (
              <div className="space-y-6">
                {visibleGroups.map((group) => (
                  <div key={group.key} className="space-y-2">
                    <div className="flex flex-wrap items-baseline gap-2">
                      <h2 className="text-base font-semibold">{group.name}</h2>
                      <span className="text-xs text-muted-foreground">
                        {group.code || "—"} · {group.tasks.length} 条 ·{" "}
                        {
                          group.tasks.filter(
                            (task) => taskOperability(task).operable,
                          ).length
                        }{" "}
                        条可处理
                      </span>
                      {group.code && (
                        <Button
                          variant="outline"
                          size="sm"
                          className="ml-auto"
                          onClick={() => setSettingsCode(group.code)}
                        >
                          字段设置
                        </Button>
                      )}
                    </div>
                    <DefinitionTable
                      key={`${group.key}:${tableVersion}`}
                      tasks={group.tasks}
                      definitionKey={group.key}
                      view={views[group.code]}
                      accessToken={token}
                      selected={selected}
                      onSelect={(tableTasks, nextSelection) =>
                        replaceTableSelection(tableTasks, nextSelection)
                      }
                      onFilteredTasksChange={(key, ids) =>
                        setTableFilteredIds((previous) => {
                          const current = previous[key];
                          if (
                            current &&
                            current.length === ids.length &&
                            current.every((id, index) => id === ids[index])
                          ) {
                            return previous;
                          }
                          return { ...previous, [key]: ids };
                        })
                      }
                    />
                  </div>
                ))}
              </div>
            )}
          </section>
        </div>
      </main>

      <BatchViewSettingsDrawer
        open={settingsCode !== null}
        approvalCode={settingsCode ?? ""}
        accessToken={token}
        fields={formColumns(
          tasks.filter((task) => task.definitionCode === settingsCode),
        )}
        onClose={() => setSettingsCode(null)}
        onSaved={() => {
          setSettingsCode(null);
          void loadTasks();
        }}
      />
      <Dialog
        open={reauthOpen}
        onOpenChange={(next) => {
          if (!next && !authorizing) setReauthOpen(false);
        }}
      >
        <DialogContent
          aria-label="重新授权飞书"
          className="sm:max-w-sm"
          showCloseButton={!authorizing}
        >
          <DialogHeader>
            <DialogTitle>重新授权飞书</DialogTitle>
            <DialogDescription>
              飞书登录状态已过期。请输入当前账号密码确认身份，随后跳转飞书重新授权。
            </DialogDescription>
          </DialogHeader>
          <form
            className="space-y-3"
            onSubmit={(event) => {
              event.preventDefault();
              void submitReauthorize();
            }}
          >
            <Input
              autoFocus
              type="password"
              aria-label="当前账号密码"
              placeholder="当前账号密码"
              autoComplete="current-password"
              value={reauthPassword}
              disabled={authorizing}
              onChange={(event) => setReauthPassword(event.target.value)}
            />
            {reauthError && (
              <p role="alert" className="text-sm text-destructive">
                {reauthError}
              </p>
            )}
            <DialogFooter>
              <Button
                type="button"
                variant="ghost"
                disabled={authorizing}
                onClick={() => setReauthOpen(false)}
              >
                取消
              </Button>
              <Button type="submit" disabled={authorizing}>
                {authorizing ? "跳转中…" : "确认并重新授权"}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      <BatchActionDialog
        open={action !== null}
        action={action}
        count={selectedTasks.length}
        tasks={selectedTasks}
        accessToken={token}
        submitting={submittingAction}
        errorMessage={actionError}
        onCancel={() => {
          if (submittingAction) return;
          setAction(null);
          setActionError("");
        }}
        onConfirm={(params) => void confirmAction(params)}
      />
      <BatchJobDialog
        open={job !== null}
        jobId={job?.id ?? null}
        actionLabel={job?.label ?? "批量审批"}
        accessToken={token}
        retrying={retrying}
        onClose={closeJob}
        onRetryFailed={(items) => void retryFailed(items)}
      />
    </div>
  );
}
