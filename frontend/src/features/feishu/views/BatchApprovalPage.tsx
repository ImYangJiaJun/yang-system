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
import { Dialog, DialogContent, DialogTitle } from "@/shared/ui/dialog";
import { Skeleton } from "@/shared/ui/skeleton";
import { formatProductDateTime } from "@/shared/lib/product-locale";
import { useToast } from "@/shared/lib/toast";

import {
  BATCH_ACTION_LABELS,
  createBatchJob,
  describeBatchError,
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
  if (fieldType === "fieldlist" && Array.isArray(field.value)) {
    return `明细 ${field.value.length} 行`;
  }
  if (fieldType === "address" && typeof field.value === "string") {
    return field.value.split("/").join(" / ");
  }
  return fieldValueText(field.value);
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
  return (
    <span className="inline-flex items-center gap-1">
      <span>{label}</span>
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
          <div className="max-w-56">
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
        <div className="max-w-52">
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
          <span className="block max-w-72 truncate text-xs" title={summary}>
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
      defaultSortOrder: "descend",
      filters: uniqueFilters(tasks.map((task) => dateKey(task.createdAt))),
      onFilter: (value, task) => dateKey(task.createdAt) === String(value),
      render: (_, task) => (
        <span className="tabular-nums" title={timeText(task.createdAt)}>
          {timeText(task.createdAt)}
        </span>
      ),
    },
    {
      title: columnTitle("API 操作", "instance"),
      key: "apiOperate",
      width: 110,
      filters: [
        { text: "支持", value: "supported" },
        { text: "不支持", value: "unsupported" },
      ],
      onFilter: (value, task) =>
        value === "supported"
          ? task.supportApiOperate
          : !task.supportApiOperate,
      render: (_, task) => (
        <StatusBadge tone={task.supportApiOperate ? "positive" : "warning"}>
          {task.supportApiOperate ? "支持" : "不支持"}
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
          <span className="block max-w-48 truncate text-xs" title={text}>
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

function DefinitionTable({
  tasks,
  view,
  selected,
  onSelect,
}: {
  tasks: BatchTask[];
  view?: BatchViewConfig;
  selected: SelectedMap;
  onSelect: (tableTasks: BatchTask[], selectedTasks: BatchTask[]) => void;
}) {
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

  return (
    <ConfigProvider locale={zhCN}>
      <Table<BatchTask>
        rowKey="taskId"
        size="small"
        bordered
        tableLayout="fixed"
        columns={columns}
        components={TABLE_COMPONENTS}
        dataSource={tasks}
        scroll={{ x: "max-content" }}
        pagination={{
          pageSize: 20,
          showSizeChanger: true,
          hideOnSinglePage: true,
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

  const [definitionGroup, setDefinitionGroup] = useState("all");
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
        } else {
          if (isFeishuScopeError(cause)) setScopeError(true);
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
  const visibleGroups = useMemo(
    () =>
      definitionGroup === "all"
        ? groups
        : groups.filter((group) => group.key === definitionGroup),
    [definitionGroup, groups],
  );
  const visibleTaskCount = useMemo(
    () => visibleGroups.reduce((total, group) => total + group.tasks.length, 0),
    [visibleGroups],
  );
  const selectedTasks = useMemo(() => Object.values(selected), [selected]);

  const resetFilters = () => {
    setDefinitionGroup("all");
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

  const reauthorize = async () => {
    if (authorizing) return;
    const password = window.prompt("请输入当前账号密码以确认身份");
    if (!password) return;
    setAuthorizing(true);
    try {
      const url = await requestFeishuAuthorizationUrl(token, password);
      window.location.assign(url);
    } catch (cause) {
      toast.error(describeBatchError(cause));
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
              处理你自己的飞书待办。写操作由后台逐条执行，不自动重试。
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
            className="w-full shrink-0 space-y-1 md:w-60"
          >
            <p className="px-2 text-xs font-medium text-muted-foreground">
              按审批定义
            </p>
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
              <span className="tabular-nums">{tasks.length}</span>
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
                <span className="tabular-nums">{group.tasks.length}</span>
              </button>
            ))}
          </aside>

          <section className="min-w-0 flex-1 space-y-3">
            <div className="flex flex-wrap items-center gap-2 rounded-lg border border-border bg-card px-3 py-2">
              <span className="text-sm text-muted-foreground">
                已选 {selectedTasks.length} 条 · 共 {visibleTaskCount} 条
              </span>
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
                <p className="font-medium">该审批定义暂无待办</p>
                <Button variant="outline" size="sm" onClick={resetFilters}>
                  查看全部待办
                </Button>
              </div>
            ) : (
              <div className="space-y-6">
                {visibleGroups.map((group) => (
                  <div key={group.key} className="space-y-2">
                    <div className="flex flex-wrap items-baseline gap-2">
                      <h2 className="text-base font-semibold">{group.name}</h2>
                      <span className="text-xs text-muted-foreground">
                        {group.code || "—"} · {group.tasks.length} 条
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
                      tasks={group.tasks}
                      view={views[group.code]}
                      selected={selected}
                      onSelect={(tableTasks, nextSelection) =>
                        replaceTableSelection(tableTasks, nextSelection)
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
      <BatchActionDialog
        open={action !== null}
        action={action}
        count={selectedTasks.length}
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
