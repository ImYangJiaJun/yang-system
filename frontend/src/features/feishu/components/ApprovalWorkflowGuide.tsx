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
            建配置时系统会自动签发一份**工作流访问
            Token**：详情页的「工作流接入」区可以复制派发链接与访问
            Token。这两样是飞书工作流向本服务发起派发请求的凭据，按配置隔离（每条配置各自一份）。
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
              JSON，请求地址填入详情页的「派发链接」（地址里已带配置标识，不需要再拼），请求头填
              Content-Type: application/json。
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
            label="派发链接（POST）"
            value={`${window.location.origin}/api/v1/feishu/approval/dispatch/<配置标识>`}
            copyLabel="复制派发链接"
            hint="把 <配置标识> 替换为详情页「工作流接入」区展示的配置标识（一段随机串）。地址按当前控制台域名生成；飞书服务器必须能够访问。"
          />
          <p className="text-muted-foreground">
            请求地址里已经包含配置标识（服务端据此定位这条配置），请求体里只需要带上访问
            Token ——Token
            由服务端建配置时签发，通过详情页「工作流接入」区查看/轮换。旧版「管理
            Token + 请求体里填坐标」的鉴权方式已废弃，不再支持。
          </p>
        </section>

        <section className="space-y-2">
          <h2 className="font-medium">4. 请求参数</h2>
          <dl className="grid gap-x-4 gap-y-2 sm:grid-cols-[max-content_1fr]">
            <dt>
              <code>token</code>
            </dt>
            <dd>
              必填。这条配置的访问
              Token（详情页「工作流接入」区查看/复制；轮换后必须把新值粘回工作流）。
            </dd>
            <dt>
              <code>record_id</code>
            </dt>
            <dd>
              单行派发必填，使用触发记录的
              ID；批量派发完全省略此键，不要填写空字符串。
            </dd>
            <dt>
              <code>applicant</code>
            </dt>
            <dd>
              可选。发起人**姓名**（不是
              open_id）。传了它，系统会用飞书用户身份去通讯录解析出 open_id
              再提单；解析失败会直接报错，不会停在等待态。
              <strong>不传</strong>
              则回退读表格里的申请人员列——两种都缺时记录停在 「缺少申请人」。
            </dd>
            <dt>
              <code>requested_by</code>
            </dt>
            <dd>
              可选。触发人标识，**仅用于派发记录**（落「派发记录」页），不参与提单；
              最多 128 个字符，省略时记录为 feishu-workflow。
            </dd>
          </dl>
          <p className="text-muted-foreground">
            派发地址里的配置标识与请求体里的访问 Token
            是这套鉴权的全部——不再需要请求体里填任何多维表格坐标（base_token /
            table_id）或审批配置字段（approval_code
            等），那些在控制台建配置时已经配好了。
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
                      token: "<访问 Token>",
                      record_id: "<触发记录 ID>",
                      applicant: "<发起人姓名>",
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
                      token: "<访问 Token>",
                      applicant: "<发起人姓名>",
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
            替换所有占位符。记录
            ID、发起人与触发人请插入工作流动态变量，不要把占位
            文字作为实际值发送。使用工作流 JSON 模板时，对应引用为
            $.step_btn.recordId、$.step_btn.user（step_btn 为触发节点 ID）。
            <strong>applicant 与 requested_by 都要用 $.step_btn.user</strong>
            ——该变量渲染出的是<strong>姓名</strong>（真机核实），正合 applicant
            所需； 两个字段都需要字符串，不要传对象。
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
            若提示「配置不存在」，检查派发链接里的配置标识是否正确；若提示「Token
            无效」，说明轮换过 Token
            而工作流里还是旧值，去详情页重新复制；若提示「尚未签发访问
            Token」，说明这条配置建好后还没签发过凭据（异常状态），联系管理员；若处于等待态，补齐申请人及审批必填字段后再查看处理结果。
          </p>
        </section>
      </div>
    </details>
  );
}
