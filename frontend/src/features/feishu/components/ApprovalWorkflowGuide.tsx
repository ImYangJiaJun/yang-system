import { CopyField } from "./CopyField";

export function ApprovalWorkflowGuide() {
  return (
    <details className="rounded-lg border border-border bg-muted/20">
      <summary className="cursor-pointer rounded-lg px-4 py-3 text-sm font-medium focus-visible:outline-2 focus-visible:outline-ring">
        多维表格工作流配置指引
      </summary>
      <div className="space-y-5 border-t border-border p-4 text-sm">
        <section className="space-y-2">
          <h2 className="font-medium">1. 准备审批派发配置</h2>
          <p>
            先通过本页「新建配置」选择多维表格、数据表、审批
            Code、申请人员列和审批编号回填列，确认配置已启用。表格列与审批控件的映射可在「映射明细」中查看。
          </p>
          <p className="text-muted-foreground">
            飞书应用需具有读取表格、更新记录和创建审批的权限，且能访问目标多维表格与审批定义。申请人取自记录中的人员列；触发按钮的人只记入派发记录。
          </p>
        </section>

        <section className="space-y-2">
          <h2 className="font-medium">2. 在多维表格中创建工作流</h2>
          <ol className="list-decimal space-y-2 pl-5">
            <li>
              打开目标多维表格的「工作流 /
              自动化」，新建工作流。单行派发选择「点击按钮」触发并绑定数据表的按钮字段；批量派发使用页面按钮触发。
            </li>
            <li>
              添加「发送 HTTP 请求」操作，请求方法选择 POST，请求体选择原始
              JSON，填写下方地址、请求头与请求体。
            </li>
            <li>
              单行请求的 record_id 从触发节点选择「记录
              ID」动态变量；requested_by
              可选择触发人标识。完成后保存并启用工作流，点击按钮测试。
            </li>
          </ol>
        </section>

        <section className="space-y-2">
          <h2 className="font-medium">3. 请求地址与鉴权</h2>
          <CopyField
            label="请求地址（POST）"
            value={`${window.location.origin}/api/v1/feishu/approval/dispatch`}
            copyLabel="复制请求地址"
            hint="地址按当前控制台域名生成；飞书服务器必须能够访问。如果当前是 localhost、内网或开发代理地址，请替换为实际部署的公网 API 地址，并保留接口路径。"
          />
          <pre className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs select-all">
            <code>
              {
                "Content-Type: application/json\nAuthorization: Bearer <管理 Token>"
              }
            </code>
          </pre>
          <p className="text-muted-foreground">
            管理 Token 由运维提供，对应服务端
            feishu.management_api_token；不能用登录 Token、飞书
            tenant_access_token 或外部选项的数据源 Token
            替代。页面仅展示占位符，请在工作流请求头中填入真实值。
          </p>
        </section>

        <section className="space-y-2">
          <h2 className="font-medium">4. 请求参数</h2>
          <dl className="grid gap-x-4 gap-y-2 sm:grid-cols-[max-content_1fr]">
            <dt>
              <code>base_token</code>
            </dt>
            <dd>必填。目标多维表格 Token，从本页配置的坐标中复制。</dd>
            <dt>
              <code>table_id</code>
            </dt>
            <dd>必填。目标数据表 ID，从同一配置的坐标中复制。</dd>
            <dt>
              <code>record_id</code>
            </dt>
            <dd>
              单行派发必填，使用触发记录的
              ID；批量派发完全省略此键，不要填写空字符串。
            </dd>
            <dt>
              <code>requested_by</code>
            </dt>
            <dd>
              可选。触发人标识，仅用于派发记录，最多 128 个字符；省略时记录为
              feishu-workflow。
            </dd>
            <dt>
              <code>approval_code</code>
            </dt>
            <dd>
              首次调用自动建配置时填写目标审批定义
              Code；已在本页建好配置时省略。
            </dd>
            <dt>
              <code>applicant_field</code>
            </dt>
            <dd>首次建配置时填写申请人员列的字段 ID 或列名。</dd>
            <dt>
              <code>backfill_field</code>
            </dt>
            <dd>首次建配置时填写审批编号回填列的字段 ID 或列名。</dd>
          </dl>
          <p className="text-muted-foreground">
            首次自动建配置的后三个参数必须同时提供；已有配置时这些值会被忽略。建议先在本页完成配置，再使用下面的最小请求体。
          </p>
          <div className="grid gap-3 lg:grid-cols-2">
            <div className="min-w-0 space-y-2">
              <h3 className="font-medium">单行按钮：派发当前记录</h3>
              <pre
                aria-label="单行派发请求体"
                className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs select-all"
              >
                <code>
                  {JSON.stringify(
                    {
                      base_token: "<多维表格 Token>",
                      table_id: "<数据表 ID>",
                      record_id: "<触发记录 ID>",
                      requested_by: "<触发人标识>",
                    },
                    null,
                    2,
                  )}
                </code>
              </pre>
            </div>
            <div className="min-w-0 space-y-2">
              <h3 className="font-medium">页面按钮：受理批量派发</h3>
              <pre
                aria-label="批量派发请求体"
                className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs select-all"
              >
                <code>
                  {JSON.stringify(
                    {
                      base_token: "<多维表格 Token>",
                      table_id: "<数据表 ID>",
                      requested_by: "<触发人标识>",
                    },
                    null,
                    2,
                  )}
                </code>
              </pre>
            </div>
          </div>
          <p className="text-muted-foreground">
            替换所有占位符。记录 ID
            和触发人请插入工作流动态变量，不要把占位文字作为实际值发送。使用工作流
            JSON 模板时，对应引用为 $.step_btn.recordId 与
            $.step_btn.user（step_btn 为触发节点 ID）；requested_by
            需要字符串标识，不使用对象，可直接省略。
          </p>
          <p className="rounded-md border border-border bg-muted/50 p-3">
            省略 record_id
            会唤醒所有启用配置的待处理队列，并非只处理请求中指定的数据表；后台处理回填列为空的记录。批量按钮请仅在需要处理整个队列时使用。
          </p>
        </section>

        <section className="space-y-2">
          <h2 className="font-medium">5. 测试与查看结果</h2>
          <p>
            先用一条申请人和必填字段齐全、回填列为空的记录测试。请求后打开「派发记录」查看结果与请求
            /
            返回原文；批量受理可展开查看任务，最终审批编号写回多维表格的回填列。
          </p>
          <p className="text-muted-foreground">
            响应使用 code / message / data 信封：检查 code 是否为 0，再看
            data.accepted、data.message 和 data.serial_number。accepted 为 true
            也可能表示等待数据补齐或批量已受理，不能据此判定审批已创建；以审批编号回填和任务状态为准。HTTP
            200 或工作流节点显示成功也不代表派发成功。
          </p>
          <p className="text-muted-foreground">
            若没有派发记录，先检查地址是否能被飞书访问、管理 Token
            和工作流执行日志；若提示未配置，检查坐标和启用状态；若处于等待态，补齐申请人及审批必填字段后再查看处理结果。
          </p>
        </section>
      </div>
    </details>
  );
}
