/**
 * xlsx 文件导入向导：填名称 + 选文件 → 勾列 → 配源标识与父列 → 创建并导入。
 *
 * # 四步各自为什么存在
 *
 * 1. **填名称 + 选文件**：文件先拿去**探表头**（只读表头，不写库）。表头是从文件里
 *    唯一能机械读出来的事实，「取数列」靠它才有的勾。
 * 2. **勾列**：哪些列当外部选项的取值来源是**业务判断**，只能由人给。一列都不勾
 *    建出来的数据源不会有任何绑定、不会出数，所以这一步必须至少勾一列。
 * 3. **配源标识与父列**：`source_key` 进出站路由、全局唯一、创建后不可改；
 *    父列决定级联，父必须是**同源内也勾了**的另一列（设计 A11）。
 * 4. **创建并导入**：先建源（配置）再上传同一批文件（数据）——见下面的「两个请求」。
 *
 * # 三个契约事实（写错了在真实使用里才会炸）
 *
 * - **列名即身份**：`field_id` 与 `field_name` 都写列名。`field_name` 是绑定行上的
 *   **展示名**，审批外部选项装配按它给出候选列；漏了（或只有空白）时服务端
 *   **宽容跳过**这一条绑定（告警，不报错），代价是那个控件**少一个候选列**——
 *   不是整批失败、范围也不是全表。
 * - **`source_key` 不能直接用中文列名**：它要求 ASCII `[a-z0-9_]`、首字节小写字母、
 *   1..=64 字节，且是全局唯一的出站路由键。所以默认值由列号派生（[`fallbackSourceKey`]）
 *   并**避开已知占用的那些**（`col_2` 被占就顺延 `col_2_2`），而不是拿列名硬转。
 * - **服务端零暂存（D11）**：第 1 步的文件只用于探表头，服务端读完即丢。`File` 对象
 *   **留在本组件的 state 里**，第 4 步重传同一批——用户只选一次文件。
 *
 * # 提交为什么是两个请求，且失败不回滚
 *
 * 建源 + N 条绑定是**配置**，导入是**数据**，两者各自可重试。所以顺序固定为
 * `createTable`（JSON，拿到 `datasource_id`）→ `importFiles`（multipart）。第 2 步失败时
 * **不回滚数据源**（`create_datasource_table` 没有幂等键，回滚再建会撞 title 重名），
 * 而是让重试只重发导入那一步。
 *
 * # 数据从哪来
 *
 * 一律走注入的 `client`（真实实现 `api.ts::useXlsxImportClient`），组件不摸会话与目录，
 * 于是它可以脱离整棵应用壳被渲染与测试。
 */

import { useEffect, useId, useMemo, useState } from "react";

import { Button } from "@/shared/ui/button";
import { Checkbox } from "@/shared/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/ui/dialog";
import { Input } from "@/shared/ui/input";
import { Label } from "@/shared/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/shared/ui/table";

import type {
  CreatedTable,
  CreateTableSubmission,
  XlsxHeaderProbe,
  XlsxImportClient,
  XlsxImportReport,
} from "../api";
import { describeImportProgress, useImportProgress } from "../import-progress";
import { isValidSourceKey } from "../types";

/// Radix Select 不接受空串作为 item 的 value，所以「无父」另给一个哨兵值。
const NO_PARENT = "__none__";

const THEAD = "text-xs text-muted-foreground";

const SOURCE_KEY_HELP =
  "源标识进接口 URL 路径段，全局唯一、创建后不可修改。默认按列号派生（col_2 / col_5 …）；" +
  "这个列号已经被别的数据源占用时，会自动顺延到下一个没被占用的（col_2_2 …）。可以直接用，也可以改。";

const SOURCE_KEY_WARNING =
  "改这一栏之前想清楚：源标识进的是那个审批控件的外部选项地址。换一个就等于换了地址——" +
  "必须回审批后台把控件里的外部选项地址一并改掉，否则它会立刻取不到任何选项。";

/// 从列号派生一个合法的 `source_key`，**并避开已知占用的那些**。
///
/// 后端要求 1..=64 字节、首字节小写 ASCII 字母、其余 `[a-z0-9_]` —— 中文列名直接拿去当
/// `source_key` 会被拒，而「把中文转成 pinyin」这类猜测不该由一个配置向导做（同一立场见
/// 设计 A14：只留机械可判定的规则）。所以初值取**列号**：`col_2` 这种键稳定、可读、
/// 且永远落在这个字符集里，用户可以改。
///
/// **为什么必须顺延**：`source_key` 是**全表唯一**的路由键，而列号在每份文件里都是同样
/// 那几个小整数——第二条 xlsx 源只要勾同列号，默认值就与第一条逐字相撞，界面上却只报
/// 「已被占用」、不给可用值（用户只能自己编四个键）。这里按已知集合顺延到第一个空闲的
/// 变体（`col_2` → `col_2_2`）。
///
/// **已知集合只是提示**：`takenSourceKeys` 握着的是列表**当前这一页**（搜索/筛选/翻页都
/// 会改变它），所以这里买到的是「别让默认值生来就撞」，不是唯一性保证——真撞了由服务端
/// 点名拒（400 带键值），唯一索引仍是最终权威。
function freeSourceKey(base: string, taken: ReadonlySet<string>): string {
  if (!taken.has(base)) return base;
  for (let n = 2; n <= 1000; n += 1) {
    const candidate = `${base}_${n}`;
    // 候选自身必须仍然合法：`col_2` 这类基底很短，但 `_1000` 会把它推到 64 字节附近。
    if (candidate.length > 64) break;
    if (!taken.has(candidate)) return candidate;
  }
  // 全被占：退回基底，让预检当场报出来——不编造一个前端会判「不是合法源标识」的建议。
  return base;
}

function fallbackSourceKey(
  columnIndex: number,
  taken: ReadonlySet<string>,
): string {
  return freeSourceKey(`col_${columnIndex}`, taken);
}

/// 表头下沉的提示文案（R18）。
///
/// `read_header` 的规则是「表头 = 第一个非全空行」，所以第 1 行整行为空时它会**静默**
/// 用到第 2 行。那个设计被判为可接受，前提正是**用户能看见这件事发生过**——否则
/// 「表头怎么变成了数据的第一行」只能靠事后翻文件对。
function headerRowNote(headerRow: number): string {
  return (
    `表头在第 ${headerRow} 行（不是第 1 行）：前 ${headerRow - 1} 行整行为空，` +
    `解析器按「第一个非全空行」读表头，于是它下沉到了第 ${headerRow} 行。` +
    `导入时也按这一行跳表头。`
  );
}

type Step = 1 | 2 | 3 | 4;

/// 一条勾选列的配置。与多维表格向导的 `RowConfig` 同形（同一个后端契约）。
type RowConfig = {
  sourceKey: string;
  parentFieldId: string | null;
};

export type XlsxImportWizardProps = {
  client: XlsxImportClient;
  /// 库里**已经存在**的源标识（`source_key` 是全局唯一索引，不是每条数据源各自一份）。
  ///
  /// 为什么必须由调用方喂进来：默认值是从**列号**派生的（`col_2` / `col_5`），而列号在
  /// 每份文件里都是那几样小整数——**第二条** xlsx 源只要勾了与第一条同列号的列，默认值
  /// 就与它逐字相撞。有了这份集合，[`freeSourceKey`] 才能把默认值顺延到没用过的那一个
  /// （`col_2` → `col_2_2`），而不是让用户自己编四个键。
  ///
  /// 调用方（`DatasourceListPage`）握着的只有**当前那一页**数据源，所以这是
  /// 「别让默认值生来就撞」而不是完整校验——漏网的那一发到提交时才失败，由服务端
  /// 点名是哪个键（`ParamInvalid`，400）。
  takenSourceKeys?: ReadonlySet<string>;
  /// 关掉向导（用户点了取消、或关掉对话框）。
  onCancel: () => void;
  /// 建源与导入**都成功**之后调用。调用方通常据此回读列表并关掉向导。
  ///
  /// 一并交回那次**提交物**：回执里要写的是用户刚填的名称，而它是向导的内部状态，
  /// 调用方从响应里读不到（响应只回 `datasource_id` 与逐字段凭据）。
  onSubmitted?: (
    created: CreatedTable,
    submission: CreateTableSubmission,
  ) => void;
  open?: boolean;
};

/// 没传 `takenSourceKeys` 时的空集合。**模块级常量**：写成解构默认值
/// （`takenSourceKeys = new Set()`）会让每次渲染都是一个新对象，下面的
/// `useMemo` 依赖每次都变。
const NO_TAKEN_KEYS: ReadonlySet<string> = new Set();

export function XlsxImportWizard({
  client,
  takenSourceKeys = NO_TAKEN_KEYS,
  onCancel,
  onSubmitted,
  open = true,
}: XlsxImportWizardProps) {
  const idPrefix = useId();
  const [step, setStep] = useState<Step>(1);

  const [title, setTitle] = useState("");
  /// **File 对象留在这里**：服务端零暂存，第 4 步要重传同一批（用户只选一次）。
  const [files, setFiles] = useState<File[]>([]);
  const [probe, setProbe] = useState<XlsxHeaderProbe | null>(null);
  /// 勾选的**列名**。列名就是这条绑定的身份（`field_id` = `field_name` = 列名）。
  const [selected, setSelected] = useState<string[]>([]);
  const [configs, setConfigs] = useState<Record<string, RowConfig>>({});

  const [probing, setProbing] = useState(false);
  const [probeError, setProbeError] = useState<string | null>(null);
  /// 探表头那组说明是不是**已经不对应当前这批文件**了。
  ///
  /// 只在冻结后换重试文件时置起：那时「要导入的文件」已经是新名字，而 probe 的结论
  /// 说的是第一次那批——两行并排摆着，看上去像换文件没生效。换文件不重探（配置已冻结，
  /// 重探也改不了任何绑定），所以只能说清它是哪一批文件的结论。
  const [probeStale, setProbeStale] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [submitError, setSubmitError] = useState<string | null>(null);
  /// 建源的结果。**非 null 就代表数据源已经在库里了**——重试只重发导入那一步，
  /// 不会再建第二个（建源没有幂等键，`title` 会重名）。
  const [created, setCreated] = useState<CreatedTable | null>(null);
  /// **真发出去的那份提交物**（`createTable` 收到的同一个对象）。
  ///
  /// 交给调用方的必须是它，而不是「界面上现在长什么样」——见 [`frozen`]。
  const [sentSubmission, setSentSubmission] =
    useState<CreateTableSubmission | null>(null);
  const [report, setReport] = useState<XlsxImportReport | null>(null);

  /// 提交中的实时进度。**只在「导入那一步」轮询**：建源阶段数据源还不存在
  /// （`created === null`），拿不到 id，也没什么可报的（见提交里那句「创建数据源与绑定」）。
  const progress = useImportProgress(
    client,
    submitting && created !== null ? created.datasourceId : null,
  );

  /// 数据源一旦建好，**配置就冻结**：列、源标识、父列、名称都已经写进库了，
  /// 界面上再改也发不出去——那不是「下次生效」，而是**静默丢弃**（用户以为自己
  /// 改成功了，导入跑的却是第一次那份）。所以冻结之后：配置只读、没有回退入口、
  /// 交回调用方的提交物取自 [`sentSubmission`]；**只有文件还能换**（第一次导入失败后
  /// 换一批重试是合理需求，原文件本身就可能是坏的）。
  const frozen = created !== null;

  /// **导入成功之后关掉向导 = 这一次到此为止**，把状态清干净。
  ///
  /// 组件在列表页是**常驻挂载**的（`open` 只是个开关），不挂载/卸载就不会自动重置：
  /// 成功那一屏（`report !== null`）里主按钮是「已导入」、上一步禁用、重试文件输入不
  /// 渲染——关掉再打开还是这一屏，**同一次页面会话里第二条 xlsx 源建不出来**，只能
  /// 刷新整页。而刷新后重开向导，正是「默认键与已有源撞车」那四条错误的现场。
  ///
  /// **只清成功这一态**：`report === null`（建源成功但导入失败）那一屏是**活的**——
  /// 文件和绑定都还在状态里，关掉再打开能直接点「重试导入」，清掉反而要重选一次文件。
  useEffect(() => {
    if (open || report === null) return;
    setStep(1);
    setTitle("");
    setFiles([]);
    setProbe(null);
    setSelected([]);
    setConfigs({});
    setProbing(false);
    setProbeError(null);
    setProbeStale(false);
    setSubmitting(false);
    setSubmitError(null);
    setCreated(null);
    setSentSubmission(null);
    setReport(null);
  }, [open, report]);

  /// 勾选顺序无关紧要，展示与提交一律按**表头里列的顺序**——界面看到的行序与落到
  /// 后端的行序一致，排查时才不会对不上。
  const chosenColumns = useMemo(
    () =>
      (probe?.columns ?? []).filter((column) => selected.includes(column.name)),
    [probe, selected],
  );

  /// 按**列名**索引的源标识问题。用 Map 而不是数组：文案要 join 成一句，而绑定表还要
  /// 拿它把出错的那一行标出来（`aria-invalid`）——数组只能定位到「第几条」。
  ///
  /// 遍历的是 `chosenColumns`（表头列序）而不是 `selected`（勾选顺序）：四个问题摆出来时
  /// 顺序与表格里的行序一致，才不用来回对。
  const keyIssues = useMemo<Map<string, string>>(() => {
    // **冻结之后这道预检必须停掉。** 配置已经写进库了，列表回读回来的
    // `takenSourceKeys` 里正好有**它自己那几个键**——于是每一条都报「已被占用」，
    // 而 `canSubmit` 拿它去禁「重试导入」。冻结态的输入框是只读的，改不了，
    // 用户就被自己的键永久按死在失败那一屏。撞库的窗口在第一轮提交时已经过去了。
    if (frozen) {
      return new Map();
    }
    const seen = new Map<string, number>();
    for (const column of chosenColumns) {
      const key = configs[column.name]?.sourceKey.trim() ?? "";
      seen.set(key, (seen.get(key) ?? 0) + 1);
    }
    const issues = new Map<string, string>();
    for (const column of chosenColumns) {
      const key = configs[column.name]?.sourceKey.trim() ?? "";
      if (!isValidSourceKey(key)) {
        issues.set(
          column.name,
          `「${key}」不是合法源标识：必须是小写字母开头的 [a-z0-9_]（只含小写字母、数字与下划线），最长 64 字节。`,
        );
      } else if ((seen.get(key) ?? 0) > 1) {
        // 唯一索引在库上，撞了会以 ParamInvalid 冒泡——与其让创建整个失败，
        // 不如在这里说清是哪一行。
        issues.set(
          column.name,
          `源标识重复：${key}（它进 URL 路径段，必须全局唯一）。`,
        );
      } else if (takenSourceKeys.has(key)) {
        // **跨数据源**的重复：`source_key` 是全局唯一索引。默认值已经会顺延避让
        // （[`freeSourceKey`]），命中这一条说明用户**手改**成了某个在用的键——
        // 或那个键在别的页上，本地这份只是列表当前页（挡看得见的那批）。
        // 库上那一发要到「创建并导入」按下去才失败，所以在这里先说。
        issues.set(
          column.name,
          `源标识已被占用：${key}（它进 URL 路径段，全局唯一——已有别的字段绑定在用这个标识，停用的绑定也算占用）。`,
        );
      }
    }
    return issues;
  }, [frozen, chosenColumns, configs, takenSourceKeys]);

  const canSubmit = chosenColumns.length > 0 && keyIssues.size === 0;

  function chooseFiles(next: File[]) {
    setFiles(next);
    // 换了文件，上一批的探表头结果、勾选与配置一律作废：列名即身份，对不上就是白配。
    setProbe(null);
    setProbeStale(false);
    setSelected([]);
    setConfigs({});
    setProbeError(null);
  }

  function toggle(column: XlsxHeaderProbe["columns"][number]) {
    const name = column.name;
    setSelected((previous) =>
      previous.includes(name)
        ? previous.filter((candidate) => candidate !== name)
        : [...previous, name],
    );
    setConfigs((previous) => {
      if (previous[name] !== undefined) {
        const next = { ...previous };
        delete next[name];
        // 取消勾选会把「以它为父」的行悬空：父不在勾选集合里，后端会拒（父必须是
        // 同源内勾选集合中的列）。这里顺手清掉，别把错误留到提交那一刻。
        for (const [key, config] of Object.entries(next)) {
          if (config.parentFieldId === name) {
            next[key] = { ...config, parentFieldId: null };
          }
        }
        return next;
      }
      // 默认键**只在勾选这一刻取一次**，之后不跟随 `takenSourceKeys` 变化：它是要抄进
      // 审批控件外部选项地址的可见值，在用户看不见的时候被改名比撞车更难查。
      const taken = new Set<string>([
        ...takenSourceKeys,
        ...Object.values(previous).map((config) => config.sourceKey.trim()),
      ]);
      return {
        ...previous,
        [name]: {
          sourceKey: fallbackSourceKey(column.index, taken),
          parentFieldId: null,
        },
      };
    });
  }

  async function probeHeaders() {
    setProbing(true);
    setProbeError(null);
    setProbeStale(false);
    try {
      const result = await client.probe(files);
      setSelected([]);
      setConfigs({});
      setCreated(null);
      setReport(null);
      if (result.columns.length === 0) {
        // 一列都没有 = 下一步是一个死胡同（勾不了任何列）。停在原地，把原话说出来。
        setProbe(null);
        setProbeError(
          "这份文件没读到任何列名：表头那一行是空的，或者文件不是 xlsx。",
        );
        return;
      }
      setProbe(result);
      setStep(2);
    } catch (error) {
      setProbeError(error instanceof Error ? error.message : String(error));
    } finally {
      setProbing(false);
    }
  }

  async function submit() {
    setSubmitting(true);
    setSubmitError(null);
    const submission: CreateTableSubmission = {
      title: title.trim(),
      ingestMode: "xlsx_import",
      fields: chosenColumns.map((column) => ({
        fieldId: column.name, // 列名即身份（后端 field_id 存列名）
        fieldName: column.name, // **必须同时给**，否则审批选项装配整批失败
        // xlsx 没有飞书字段类型码这回事；`createDatasourceTable` 也**不把 `type`
        // 写进请求体**（它只送 field_id / field_name / source_key / parent_field_id）。
        // 给 0 是因为 `TableWizardField.type` 的类型是 `number`（合同见 Task 12）。
        type: 0,
        sourceKey: configs[column.name]?.sourceKey.trim() ?? "",
        parentFieldId: configs[column.name]?.parentFieldId ?? null,
      })),
    };
    try {
      // 1) 建源 + N 条绑定。此时还没有数据。
      //    已经建过就**不再建第二个**：这一支只在重试时走到。
      let target = created;
      if (target === null) {
        target = await client.createTable(submission);
        setCreated(target);
        setSentSubmission(submission);
      }
      // 2) 导入。重传第 1 步那批 File（浏览器里一直留着）。
      const imported = await client.importFiles(target.datasourceId, files);
      setReport(imported);
      // 交回调用方的恒是**真发出去的那一份**（首轮就是上面这个 `submission`，
      // 重试则是 `sentSubmission`）。冻结之后两者本就相等，但写成「存下来的那份」
      // 才不会有人在将来把这里换成界面重算的一份——那是一句无声的谎。
      onSubmitted?.(target, sentSubmission ?? submission);
    } catch (error) {
      setSubmitError(error instanceof Error ? error.message : String(error));
    } finally {
      setSubmitting(false);
    }
  }

  /// 提交失败的两种情形**必须分开说**：建源失败 = 什么都没写成；建源成功而导入失败 =
  /// 数据源与绑定已经在库里了（不回滚——两者各自可重试）。把后者说成「全都失败了」，
  /// 用户会去台账里找不到东西，或以为可以重头再来一次（那会撞 title 重名）。
  const submitFailure =
    submitError === null
      ? null
      : created === null
        ? submitError
        : `数据源已建好（#${created.datasourceId}），但导入失败：${submitError}。可以重试导入——重试只会重发导入这一步，不会再建一个数据源，也不会把配置重发一遍。`;

  const alert =
    submitFailure ??
    probeError ??
    (keyIssues.size > 0 ? [...keyIssues.values()].join("；") : null);

  /// 冻结之后**必须明说为什么**：界面上的列与源标识看起来还能读懂，若不说，
  /// 用户会以为自己还能改（而改了既不生效也不报错，是最坏的一种失败）。
  const frozenNotice = !frozen ? null : (
    <p className="rounded-md border border-border bg-muted/50 px-3 py-2 text-xs">
      {`数据源已建好（#${created?.datasourceId}）：列、源标识、父列与名称都按第一次提交落定，已经写进库里了，这里不再改——要改配置请先删掉这条数据源再重建。`}
      {/* 只有还没导入成功时才谈「重试」：成功之后按钮已经是「已导入」了。 */}
      {report === null ? "下面的文件可以换一批重试导入。" : ""}
    </p>
  );

  /// 探表头读到的三件事里，有两条必须说出来（[`headerRowNote`] 与 sheet 数），
  /// 第三条是服务端**实际读到的文件**——它与用户选的是不是同一批，只有这里能看。
  ///
  /// 冻结后换过文件（[`probeStale`]）时这组说明**不再描述当前这批文件**：那时它说的
  /// 是第一次解析的结论，而「要导入的文件」已经是新的。所以明说是哪一批的结论，
  /// 并收起那句指着旧文件的「服务端读到的文件」——留着它就成了「换文件没生效」。
  const probeNotes =
    probe === null ? null : (
      <div className="space-y-1 rounded-md border border-border bg-muted/50 px-3 py-2 text-xs">
        {probeStale ? (
          <p>
            {
              "下面几条是基于首次解析的结论，已经与现在要导入的那批文件无关：配置在建源那一刻就落定了，换文件不会重读表头。要按新文件的表头重配，只能删掉这条数据源重建。"
            }
          </p>
        ) : null}
        {probe.sheets.length > 1 ? (
          <p>
            {`这份文件有 ${probe.sheets.length} 张 sheet，只读了第一张（${probe.sheetName}）：其余 sheet 的列不进这次导入。`}
          </p>
        ) : (
          <p>{`读的是 sheet「${probe.sheetName}」。`}</p>
        )}
        {probe.headerRow !== 1 ? <p>{headerRowNote(probe.headerRow)}</p> : null}
        {probeStale ? null : (
          <p>
            {`服务端读到的文件：${probe.files.map((item) => item.name).join("、") || "（无）"}`}
          </p>
        )}
      </div>
    );

  /// 每列的源标识与父列。第 3 步配、第 4 步**同一张表再出现一次**：提交那一屏必须
  /// 让人看清「马上要写进库的就是这些」，而写库是不可逆的一次请求——最后一次改错的
  /// 机会留在这里，比事后去详情页改绑定便宜得多。
  const bindingsTable = (
    <Table>
      <TableHeader>
        <TableRow>
          <TableHead>列名</TableHead>
          <TableHead className={THEAD}>列号</TableHead>
          <TableHead>源标识</TableHead>
          <TableHead>父列</TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {chosenColumns.map((column) => {
          const config = configs[column.name];
          const parentValue = config?.parentFieldId ?? null;
          return (
            <TableRow key={column.index} data-slot="binding-row">
              <TableCell className="align-top">{column.name}</TableCell>
              <TableCell className={`align-top ${THEAD}`}>
                {`第 ${column.index} 列`}
              </TableCell>
              <TableCell className="align-top">
                <Input
                  aria-label={`${column.name} 的源标识`}
                  value={config?.sourceKey ?? ""}
                  readOnly={frozen}
                  // 出问题的是**某一行**，不是「有四条错误」：alert 把四句 join 成一段，
                  // 不指行的话用户得自己拿键值回表里对。shadcn 的 Input 自带
                  // `aria-invalid:` 红框样式，一行属性就够。
                  aria-invalid={keyIssues.has(column.name)}
                  onChange={(event) => {
                    // 上一次提交是**按当时那个键**失败/成功的，键一改这句就不再描述
                    // 屏幕上的东西。只在这一处清：改父列不该顺手抹掉它。
                    setSubmitError(null);
                    setConfigs((previous) => ({
                      ...previous,
                      [column.name]: {
                        sourceKey: event.target.value,
                        parentFieldId:
                          previous[column.name]?.parentFieldId ?? null,
                      },
                    }));
                  }}
                  className="font-mono"
                  autoComplete="off"
                  spellCheck={false}
                />
              </TableCell>
              <TableCell className="align-top">
                <Select
                  value={parentValue ?? NO_PARENT}
                  disabled={frozen}
                  onValueChange={(value) =>
                    setConfigs((previous) => ({
                      ...previous,
                      [column.name]: {
                        sourceKey: previous[column.name]?.sourceKey ?? "",
                        parentFieldId: value === NO_PARENT ? null : value,
                      },
                    }))
                  }
                >
                  <SelectTrigger
                    aria-label={`${column.name} 的父列`}
                    className="w-full"
                  >
                    {/* 显式给文本：Radix 默认只在「选项渲染过」之后才把选中项的
                        文案填回触发器，而这一栏的当前值必须一眼可见。 */}
                    <SelectValue>{parentValue ?? "无父列（顶级）"}</SelectValue>
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={NO_PARENT}>无父列（顶级）</SelectItem>
                    {/* 只列**同源内已勾选**的其它列：父子关系在同源内，父不在勾选
                        集合里后端会拒（A11），自指更是无意义。 */}
                    {chosenColumns
                      .filter((candidate) => candidate.name !== column.name)
                      .map((candidate) => (
                        <SelectItem
                          key={candidate.index}
                          value={candidate.name}
                        >
                          {candidate.name}
                        </SelectItem>
                      ))}
                  </SelectContent>
                </Select>
              </TableCell>
            </TableRow>
          );
        })}
      </TableBody>
    </Table>
  );

  const receipt =
    report === null ? null : (
      <div className="space-y-1 rounded-md border border-border px-3 py-2 text-xs">
        <p>
          {`数据源 #${report.datasourceId} 已建好，导入用了 ${(report.elapsedMs / 1000).toFixed(1)} 秒。`}
        </p>
        {report.files.map((item) => (
          <p key={item.name}>{`${item.name}：读了 ${item.rowsRead} 行`}</p>
        ))}
        {report.bindings.map((binding) => (
          <p key={binding.sourceKey}>
            {`${binding.sourceKey}：读到 ${binding.fetched} 行 → ${binding.derived} 个选项，停用 ${binding.disabled} 条`}
            {/* 「停用 0 条」有两种含义，这一种是**根本没扫**（已启用选项数超过单轮
                上限）。不写出来的话它读起来就是「没什么要停用的」——而旧选项还启着。 */}
            {binding.complementSkipped === null
              ? ""
              : `｜这一轮没做补集停用（已启用 ${binding.complementSkipped} 条，超过单轮上限），改过名或删掉的旧选项仍保持启用`}
            {binding.unchanged ? "（内容没变，本轮没写库）" : ""}
            {binding.skippedReason === null
              ? ""
              : `｜这一轮跳过了它：${binding.skippedReason}`}
            {binding.anomalies.length > 0
              ? `｜异常 ${binding.anomalies.length} 行${binding.truncatedDetails ? "（还有更多没列出来）" : ""}`
              : ""}
          </p>
        ))}
      </div>
    );

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next && !submitting) onCancel();
      }}
    >
      <DialogContent className="sm:max-w-3xl" showCloseButton={!submitting}>
        <DialogHeader>
          <DialogTitle>导入 xlsx 文件</DialogTitle>
          <DialogDescription>
            上传一批表头一致的
            xlsx，勾出要作为外部选项的列，建成一条「文件导入」
            数据源并把数据导进去。服务端不出网、也没有定时同步。
          </DialogDescription>
        </DialogHeader>

        <p className="text-sm text-muted-foreground" aria-live="polite">
          {stepLabel(step)}
        </p>

        {alert === null ? null : (
          <p
            role="alert"
            className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          >
            {alert}
          </p>
        )}

        {probeNotes}
        {frozenNotice}

        {step === 1 ? (
          <div className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor={`${idPrefix}-title`}>名称</Label>
              <Input
                id={`${idPrefix}-title`}
                value={title}
                onChange={(event) => setTitle(event.target.value)}
                autoComplete="off"
              />
              <p className="text-xs text-muted-foreground">
                展示用名称，1..=100 字符。
              </p>
            </div>

            <div className="space-y-1.5">
              <Label htmlFor={`${idPrefix}-files`}>xlsx 文件</Label>
              <Input
                id={`${idPrefix}-files`}
                type="file"
                multiple
                accept=".xlsx"
                onChange={(event) =>
                  chooseFiles(Array.from(event.target.files ?? []))
                }
              />
              <p className="text-xs text-muted-foreground">
                可以一次选多份：它们的表头必须完全一致，不一致会被整批拒掉。
                文件只留在浏览器里，第 4 步重新上传同一批——不用再选一次。
              </p>
            </div>

            <div className="space-y-1.5">
              <Button
                variant="outline"
                size="sm"
                disabled={probing || title.trim() === "" || files.length === 0}
                onClick={() => void probeHeaders()}
              >
                {probing ? "正在解析…" : "解析表头"}
              </Button>
              <p className="text-xs text-muted-foreground">
                只读表头，不写库；解析成功就直接进下一步勾列。
              </p>
            </div>
          </div>
        ) : null}

        {step === 2 && probe !== null ? (
          <div className="space-y-3">
            <p className="text-xs text-muted-foreground">
              整张表的列都在这里，不按内容过滤。勾零列也能建出数据源，但它一条绑定
              都没有、不会出数，只会白占台账一行——所以至少勾一列。
            </p>
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead className="w-10">
                    <span className="sr-only">勾选</span>
                  </TableHead>
                  <TableHead>列名</TableHead>
                  <TableHead className="w-28">列号</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {probe.columns.map((column) => {
                  const checkboxId = `${idPrefix}-col-${column.index}`;
                  return (
                    <TableRow key={column.index} data-slot="column-pick-row">
                      <TableCell>
                        <Checkbox
                          id={checkboxId}
                          checked={selected.includes(column.name)}
                          onCheckedChange={() => toggle(column)}
                        />
                      </TableCell>
                      <TableCell>
                        {/* Label 的文本就是列名——勾选框的可访问名等于列名，
                            列名同时也是这条绑定的身份。 */}
                        <Label htmlFor={checkboxId} className="font-normal">
                          {column.name}
                        </Label>
                      </TableCell>
                      <TableCell className="text-xs text-muted-foreground">
                        {`第 ${column.index} 列`}
                      </TableCell>
                    </TableRow>
                  );
                })}
              </TableBody>
            </Table>
          </div>
        ) : null}

        {step === 3 ? (
          <div className="space-y-3">
            {bindingsTable}
            <p className="text-xs text-muted-foreground">{SOURCE_KEY_HELP}</p>
            <p className="rounded-md border border-border bg-muted/50 px-3 py-2 text-xs">
              {SOURCE_KEY_WARNING}
            </p>
          </div>
        ) : null}

        {step === 4 ? (
          <div className="space-y-3">
            {bindingsTable}
            <div className="space-y-1 rounded-md border border-border bg-muted/50 px-3 py-2 text-xs">
              <p>{`要导入的文件：${files.map((item) => item.name).join("、") || "（无）"}`}</p>
              <p>
                提交分两步：先建数据源与绑定（配置），再上传同一批文件导数据。
                <strong>第一步成功、第二步失败时不会回滚数据源</strong>
                ——两者各自可重试，重试只会重发导入这一步，不会再建一个数据源。
              </p>
            </div>
            {frozen && report === null ? (
              <div className="space-y-1.5">
                <Label htmlFor={`${idPrefix}-retry-files`}>
                  重试导入用的 xlsx 文件
                </Label>
                {/* 配置冻结之后**唯一还能动的**就是文件：原文件本身可能就是坏的
                    （表头不一致、列缺了），换一批重试是最常见的处置。绑定不动，
                    所以新文件的表头必须与已落定的那几列一致，否则服务端整批拒。 */}
                <Input
                  id={`${idPrefix}-retry-files`}
                  type="file"
                  multiple
                  accept=".xlsx"
                  onChange={(event) => {
                    setFiles(Array.from(event.target.files ?? []));
                    // 换了文件，探表头那组说明就不再描述当前这批（它说的是第一次解析）。
                    setProbeStale(true);
                  }}
                />
                <p className="text-xs text-muted-foreground">
                  换一批文件重试是允许的；新文件的表头必须与上面这几列一致，
                  否则服务端会整批拒掉。
                </p>
              </div>
            ) : null}
            {/* 提交中的进度行。建源那一半**报不出来**（还没有 id），只能说它正在发生；
                导入那一半才能问到阶段与计数。按钮上的「正在导入…」不动：它是文案，
                这行才是可查证的观测。 */}
            {submitting ? (
              <p role="status" className="text-xs text-muted-foreground">
                {created === null
                  ? "正在创建数据源与绑定…"
                  : describeImportProgress(progress, files)}
              </p>
            ) : null}
            {receipt}
          </div>
        ) : null}

        <DialogFooter>
          {step === 1 ? (
            <Button variant="ghost" disabled={submitting} onClick={onCancel}>
              取消
            </Button>
          ) : (
            <Button
              variant="ghost"
              // 冻结之后没有「上一步」：配置已经写进库了，退回去能看见的东西
              // 一个也改不了（只读），而第 1 步根本没有可走的出路——那只会让人
              // 以为自己漏掉了什么。要改配置只有删掉数据源重建这一条路。
              disabled={submitting || frozen}
              onClick={() =>
                setStep((current) =>
                  current === 4 ? 3 : current === 3 ? 2 : 1,
                )
              }
            >
              上一步
            </Button>
          )}

          {step === 2 ? (
            <Button disabled={selected.length === 0} onClick={() => setStep(3)}>
              下一步
            </Button>
          ) : step === 3 ? (
            <Button disabled={keyIssues.size > 0} onClick={() => setStep(4)}>
              下一步
            </Button>
          ) : step === 4 ? (
            <Button
              disabled={!canSubmit || submitting || report !== null}
              onClick={() => void submit()}
            >
              {submitting
                ? "正在导入…"
                : report !== null
                  ? "已导入"
                  : created === null
                    ? "创建并导入"
                    : "重试导入"}
            </Button>
          ) : null}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/// 四步的当前进度。**写死在文案里**比画一个进度条更实在：
/// 每一步的名字就是它要问的那件事。
function stepLabel(step: Step): string {
  switch (step) {
    case 1:
      return "第 1 步／共 4 步：填名称、选一批表头一致的 xlsx，然后解析表头。";
    case 2:
      return "第 2 步／共 4 步：勾选要作为外部选项的列（至少一列）。";
    case 3:
      return "第 3 步／共 4 步：给每列配源标识与父列。";
    default:
      return "第 4 步／共 4 步：创建数据源并导入数据。";
  }
}
