//! 批量审批计算字段配置：只读派生列，不回写飞书审批。

import { useCallback, useEffect, useMemo, useState } from "react";
import { Link } from "react-router";
import {
  Alert,
  Button,
  Form,
  Input,
  InputNumber,
  Popconfirm,
  Select,
  Space,
  Switch,
  Table,
  Typography,
  message,
} from "antd";
import type { TableColumnsType } from "antd";
import { ArrowLeft, Plus, RefreshCw } from "lucide-react";
import { useSessionCredentials } from "@/engine";
import {
  BATCH_COMPUTED_OPERATOR_LABELS,
  type BatchComputedFieldConfig,
  type BatchComputedFieldInput,
  type BatchComputedOperator,
  createBatchComputedField,
  deleteBatchComputedField,
  fetchBatchComputedFields,
  updateBatchComputedField,
} from "../batch-approval-api";
import { useApprovalWizardClient } from "../api";
import type { ApprovalWidget } from "../types";

const OPERATOR_OPTIONS = (
  Object.entries(BATCH_COMPUTED_OPERATOR_LABELS) as Array<
    [BatchComputedOperator, string]
  >
).map(([value, label]) => ({ value, label }));

type FormValues = {
  approvalCode: string;
  name: string;
  leftFieldId: string;
  operator: BatchComputedOperator;
  rightFieldId: string;
  precision: number;
  enabled: boolean;
  sortOrder: number;
};

function messageOf(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}

function widgetLabel(widget: ApprovalWidget): string {
  const type = widget.type ? `（${widget.type}）` : "";
  return `${widget.name || widget.id}${type}`;
}

export default function BatchComputedFieldsPage() {
  const { token } = useSessionCredentials();
  const wizard = useApprovalWizardClient();
  const [messageApi, contextHolder] = message.useMessage();
  const [form] = Form.useForm<FormValues>();
  const [fields, setFields] = useState<BatchComputedFieldConfig[]>([]);
  const [widgets, setWidgets] = useState<ApprovalWidget[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [editing, setEditing] = useState<BatchComputedFieldConfig | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [widgetError, setWidgetError] = useState<string | null>(null);
  const [loadingWidgets, setLoadingWidgets] = useState(false);

  const loadFields = useCallback(
    async (approvalCode?: string) => {
      setLoading(true);
      setLoadError(null);
      try {
        setFields(
          await fetchBatchComputedFields(token ?? undefined, approvalCode),
        );
      } catch (cause) {
        setLoadError(messageOf(cause));
      } finally {
        setLoading(false);
      }
    },
    [token],
  );

  useEffect(() => {
    void loadFields();
  }, [loadFields]);

  const loadWidgets = useCallback(
    async (approvalCode: string) => {
      const code = approvalCode.trim();
      if (!code) {
        messageApi.warning("请先填写审批定义 Code");
        return;
      }
      setLoadingWidgets(true);
      setWidgetError(null);
      try {
        const next = await wizard.listWidgets(code);
        setWidgets(next);
        if (next.length === 0) {
          setWidgetError("该审批定义没有返回控件");
        }
      } catch (cause) {
        setWidgets([]);
        setWidgetError(messageOf(cause));
      } finally {
        setLoadingWidgets(false);
      }
    },
    [messageApi, wizard],
  );

  const options = useMemo(() => {
    const byId = new Map(widgets.map((widget) => [widget.id, widget]));
    // 编辑已有配置时即使控件列表尚未加载，也保留中文名供下拉回显。
    for (const field of fields) {
      if (field.leftFieldId && !byId.has(field.leftFieldId)) {
        byId.set(field.leftFieldId, {
          id: field.leftFieldId,
          name: field.leftFieldName ?? field.leftFieldId,
          type: "",
          required: false,
        });
      }
      if (field.rightFieldId && !byId.has(field.rightFieldId)) {
        byId.set(field.rightFieldId, {
          id: field.rightFieldId,
          name: field.rightFieldName ?? field.rightFieldId,
          type: "",
          required: false,
        });
      }
    }
    return [...byId.values()]
      .sort((left, right) => left.name.localeCompare(right.name, "zh-CN"))
      .map((widget) => ({
        value: widget.id,
        label: widgetLabel(widget),
      }));
  }, [fields, widgets]);

  function openCreate() {
    setEditing(null);
    form.resetFields();
    form.setFieldsValue({
      approvalCode: fields[0]?.approvalCode ?? "",
      operator: "multiply",
      precision: 2,
      enabled: true,
      sortOrder: 0,
    });
    setWidgets([]);
    setWidgetError(null);
    setModalOpen(true);
  }

  function openEdit(field: BatchComputedFieldConfig) {
    setEditing(field);
    form.setFieldsValue({
      approvalCode: field.approvalCode,
      name: field.name,
      leftFieldId: field.leftFieldId,
      operator: field.operator,
      rightFieldId: field.rightFieldId,
      precision: field.precision,
      enabled: field.enabled,
      sortOrder: field.sortOrder,
    });
    setModalOpen(true);
    void loadWidgets(field.approvalCode);
  }

  async function submit() {
    const values = await form.validateFields();
    const approvalCode = values.approvalCode.trim();
    const left = widgets.find((widget) => widget.id === values.leftFieldId);
    const right = widgets.find((widget) => widget.id === values.rightFieldId);
    const input: BatchComputedFieldInput = {
      approvalCode,
      name: values.name.trim(),
      leftFieldId: values.leftFieldId,
      leftFieldName: left?.name ?? null,
      operator: values.operator,
      rightFieldId: values.rightFieldId,
      rightFieldName: right?.name ?? null,
      precision: values.precision,
      enabled: values.enabled,
      sortOrder: values.sortOrder,
    };
    setSaving(true);
    try {
      if (editing) {
        await updateBatchComputedField(editing.id, input, token ?? undefined);
        messageApi.success("计算字段已更新");
      } else {
        await createBatchComputedField(input, token ?? undefined);
        messageApi.success("计算字段已创建");
      }
      setModalOpen(false);
      await loadFields();
    } catch (cause) {
      messageApi.error(messageOf(cause));
    } finally {
      setSaving(false);
    }
  }

  async function remove(field: BatchComputedFieldConfig) {
    try {
      await deleteBatchComputedField(field.id, token ?? undefined);
      messageApi.success("计算字段已删除");
      await loadFields();
    } catch (cause) {
      messageApi.error(messageOf(cause));
    }
  }

  const columns: TableColumnsType<BatchComputedFieldConfig> = [
    {
      title: "审批定义 Code",
      dataIndex: "approvalCode",
      key: "approvalCode",
      width: 180,
      filters: [...new Set(fields.map((field) => field.approvalCode))].map(
        (code) => ({ text: code, value: code }),
      ),
      onFilter: (value, field) => field.approvalCode === String(value),
      filterSearch: true,
    },
    {
      title: "字段名",
      dataIndex: "name",
      key: "name",
      width: 180,
    },
    {
      title: "左控件",
      key: "left",
      width: 220,
      render: (_, field) => field.leftFieldName ?? field.leftFieldId,
    },
    {
      title: "运算",
      dataIndex: "operator",
      key: "operator",
      width: 90,
      render: (value: BatchComputedOperator) =>
        BATCH_COMPUTED_OPERATOR_LABELS[value] ?? value,
    },
    {
      title: "右控件",
      key: "right",
      width: 220,
      render: (_, field) => field.rightFieldName ?? field.rightFieldId,
    },
    {
      title: "小数位",
      dataIndex: "precision",
      key: "precision",
      width: 90,
      align: "right",
    },
    {
      title: "启用",
      dataIndex: "enabled",
      key: "enabled",
      width: 80,
      filters: [
        { text: "启用", value: "1" },
        { text: "停用", value: "0" },
      ],
      onFilter: (value, field) => (field.enabled ? "1" : "0") === String(value),
      render: (enabled: boolean) => (enabled ? "启用" : "停用"),
    },
    {
      title: "排序",
      dataIndex: "sortOrder",
      key: "sortOrder",
      width: 80,
      align: "right",
      sorter: (left, right) => left.sortOrder - right.sortOrder,
    },
    {
      title: "操作",
      key: "actions",
      width: 140,
      render: (_, field) => (
        <Space size="small">
          <Button type="link" size="small" onClick={() => openEdit(field)}>
            编辑
          </Button>
          <Popconfirm
            title="删除这个计算字段？"
            description="删除后批量审批表格不再显示该列。"
            okText="删除"
            cancelText="取消"
            onConfirm={() => void remove(field)}
          >
            <Button type="link" size="small" danger>
              删除
            </Button>
          </Popconfirm>
        </Space>
      ),
    },
  ];

  return (
    <div className="flex flex-col gap-5">
      {contextHolder}
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <Link
            to="/feishu/approval/configs"
            className="mb-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-4" aria-hidden="true" />
            返回审批配置
          </Link>
          <Typography.Title level={2} className="!mb-2">
            批量审批计算字段
          </Typography.Title>
          <Typography.Paragraph className="!mb-0 text-muted-foreground">
            按审批定义配置只读计算列。计算只发生在批量审批列表，不写回飞书审批；
            汇率即使来自外部数据源、控件类型不是数值，也会在运行时按数值文本解析。
          </Typography.Paragraph>
        </div>
        <Space>
          <Button
            icon={<RefreshCw className="size-4" />}
            onClick={() => void loadFields()}
          >
            刷新
          </Button>
          <Button
            type="primary"
            icon={<Plus className="size-4" />}
            onClick={openCreate}
          >
            新增字段
          </Button>
        </Space>
      </div>

      {loadError && (
        <Alert
          type="error"
          showIcon
          message="加载计算字段失败"
          description={loadError}
        />
      )}

      <Table
        rowKey="id"
        size="small"
        bordered
        loading={loading}
        columns={columns}
        dataSource={fields}
        pagination={{ pageSize: 20, showSizeChanger: true }}
        scroll={{ x: "max-content" }}
        locale={{ emptyText: "还没有计算字段" }}
      />

      <div
        className={
          modalOpen
            ? "fixed inset-0 z-50 flex items-center justify-center bg-black/40 p-4"
            : "hidden"
        }
      >
        <div className="max-h-[90vh] w-full max-w-2xl overflow-auto rounded-lg bg-background p-5 shadow-xl">
          <Typography.Title level={4} className="!mt-0">
            {editing ? "编辑计算字段" : "新增计算字段"}
          </Typography.Title>
          <Form form={form} layout="vertical" onFinish={() => void submit()}>
            <Form.Item
              name="approvalCode"
              label="审批定义 Code"
              rules={[{ required: true, message: "请填写审批定义 Code" }]}
            >
              <Input
                placeholder="例如 891B4145-..."
                onBlur={(event) => {
                  if (widgets.length === 0) {
                    void loadWidgets(event.target.value);
                  }
                }}
              />
            </Form.Item>
            <Button
              size="small"
              loading={loadingWidgets}
              onClick={() =>
                void loadWidgets(form.getFieldValue("approvalCode"))
              }
            >
              加载审批控件
            </Button>
            {widgetError && (
              <Alert
                className="mt-3"
                type="warning"
                showIcon
                message="控件加载失败，可先手动填写控件 ID"
                description={widgetError}
              />
            )}
            <Form.Item
              name="name"
              label="字段名"
              className="mt-4"
              rules={[
                { required: true, message: "请填写字段名" },
                { max: 64, message: "字段名不能超过 64 字符" },
              ]}
            >
              <Input placeholder="例如 参考人民币金额" />
            </Form.Item>
            <div className="grid gap-4 sm:grid-cols-[1fr_120px_1fr]">
              <Form.Item
                name="leftFieldId"
                label="左控件"
                rules={[{ required: true, message: "请选择左控件" }]}
              >
                <Select
                  showSearch
                  optionFilterProp="label"
                  options={options}
                  placeholder="选择金额等控件"
                  onChange={(id: string) => {
                    const widget = widgets.find((item) => item.id === id);
                    if (widget) form.setFieldValue("leftFieldId", widget.id);
                  }}
                />
              </Form.Item>
              <Form.Item
                name="operator"
                label="运算"
                rules={[{ required: true, message: "请选择运算" }]}
              >
                <Select options={OPERATOR_OPTIONS} />
              </Form.Item>
              <Form.Item
                name="rightFieldId"
                label="右控件"
                rules={[{ required: true, message: "请选择右控件" }]}
              >
                <Select
                  showSearch
                  optionFilterProp="label"
                  options={options}
                  placeholder="选择汇率等控件"
                  onChange={(id: string) => {
                    const widget = widgets.find((item) => item.id === id);
                    if (widget) form.setFieldValue("rightFieldId", widget.id);
                  }}
                />
              </Form.Item>
            </div>
            <div className="grid gap-4 sm:grid-cols-3">
              <Form.Item
                name="precision"
                label="小数位"
                rules={[{ required: true, message: "请填写小数位" }]}
              >
                <InputNumber min={0} max={6} precision={0} className="w-full" />
              </Form.Item>
              <Form.Item
                name="sortOrder"
                label="排序"
                rules={[{ required: true, message: "请填写排序" }]}
              >
                <InputNumber min={0} precision={0} className="w-full" />
              </Form.Item>
              <Form.Item name="enabled" label="启用" valuePropName="checked">
                <Switch />
              </Form.Item>
            </div>
            <Typography.Paragraph type="secondary" className="!mb-4 text-xs">
              汇率控件可以是文本或外部数据源控件；运行时若无法解析为数值，该行显示「—」。
            </Typography.Paragraph>
            <div className="flex justify-end gap-2">
              <Button onClick={() => setModalOpen(false)}>取消</Button>
              <Button type="primary" htmlType="submit" loading={saving}>
                保存
              </Button>
            </div>
          </Form>
        </div>
      </div>
    </div>
  );
}
