# 审批实例只读探针（P0 真机验证用）
#
# 用途：在不改任何数据的前提下，打印一个审批实例的 status / uuid / form /
# task_list / timeline，以及（可选）源多维表记录的当前单元格。用于验证：
#   1. 退回后在同一实例内修改并提交，GET 实例详情的 form 是否同步更新；
#   2. 退回/重提在 timeline 与 task_list 里的形态；
#   3. 附件、外部选项、日期等控件回读形态；
#   4. 台账行被锁定后，应用身份读取到的单元格内容。
#
# 用法（PowerShell 7 或 Windows PowerShell 5.1）：
#   pwsh -NoProfile -File scripts/probe_approval_instance.ps1 -InstanceCode "XXXX-..."
#   pwsh -NoProfile -File scripts/probe_approval_instance.ps1 -InstanceCode "XXXX-..." `
#        -BaseToken "appXXXX" -TableId "tblXXXX" -RecordId "recXXXX"
#
# 凭证从本地 config.toml 的 [feishu] 段读取 app_id / app_secret；
# 脚本只读，不写任何数据，不打印 app_secret 与 tenant_access_token。

param(
    [Parameter(Mandatory = $true)]
    [string]$InstanceCode,

    [string]$ConfigPath = "config.toml",

    [string]$BaseToken,
    [string]$TableId,
    [string]$RecordId
)

$ErrorActionPreference = "Stop"

function Read-FeishuCredentials {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        throw "找不到配置文件：$Path"
    }

    $section = ""
    $values = @{}
    foreach ($line in Get-Content -LiteralPath $Path) {
        $trimmed = $line.Trim()
        if ($trimmed -match '^\[(.+)\]$') {
            $section = $Matches[1].Trim().ToLowerInvariant()
            continue
        }
        if ($section -ne "feishu") { continue }
        if ($trimmed -match '^([A-Za-z0-9_]+)\s*=\s*"([^"]*)"') {
            $values[$Matches[1]] = $Matches[2]
        }
    }

    if (-not $values.ContainsKey("app_id") -or -not $values.ContainsKey("app_secret")) {
        throw "config.toml 的 [feishu] 段缺少 app_id / app_secret（出站凭证）"
    }
    return @{
        AppId     = $values["app_id"]
        AppSecret = $values["app_secret"]
    }
}

function Invoke-FeishuJson {
    param(
        [string]$Method,
        [string]$Uri,
        [hashtable]$Headers,
        $Body
    )

    $args = @{
        Method      = $Method
        Uri         = $Uri
        Headers     = $Headers
        ContentType = "application/json; charset=utf-8"
        ErrorAction = "Stop"
    }
    if ($null -ne $Body) {
        $args["Body"] = ($Body | ConvertTo-Json -Depth 10 -Compress)
    }
    $response = Invoke-RestMethod @args
    if ($null -ne $response.code -and [int]$response.code -ne 0) {
        throw "飞书接口返回错误：code=$($response.code) msg=$($response.msg) uri=$Uri"
    }
    return $response
}

$credentials = Read-FeishuCredentials -Path $ConfigPath

$tokenResponse = Invoke-FeishuJson -Method Post `
    -Uri "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal" `
    -Headers @{} `
    -Body @{ app_id = $credentials.AppId; app_secret = $credentials.AppSecret }

$tenantToken = $tokenResponse.tenant_access_token
if ([string]::IsNullOrWhiteSpace($tenantToken)) {
    throw "tenant_access_token 为空，请检查 app_id / app_secret"
}

$headers = @{ Authorization = "Bearer $tenantToken" }

$detailResponse = Invoke-FeishuJson -Method Get `
    -Uri "https://open.feishu.cn/open-apis/approval/v4/instances/$([uri]::EscapeDataString($InstanceCode))" `
    -Headers $headers `
    -Body $null

$detail = $detailResponse.data
$formJson = $null
if ($null -ne $detail.form -and -not [string]::IsNullOrWhiteSpace([string]$detail.form)) {
    try {
        $formJson = [string]$detail.form | ConvertFrom-Json
    }
    catch {
        $formJson = "FORM_PARSE_FAILED: $($_.Exception.Message)"
    }
}

$result = [ordered]@{
    fetched_at              = (Get-Date).ToString("yyyy-MM-dd HH:mm:ss")
    instance_code           = $detail.instance_code
    approval_code           = $detail.approval_code
    status                  = $detail.status
    uuid                    = $detail.uuid
    serial_number           = $detail.serial_number
    start_time              = $detail.start_time
    end_time                = $detail.end_time
    open_id                 = $detail.open_id
    reverted                = $detail.reverted
    modified_instance_code  = $detail.modified_instance_code
    reverted_instance_code  = $detail.reverted_instance_code
    form                    = $formJson
    task_list               = $detail.task_list
    timeline                = $detail.timeline
}

if (-not [string]::IsNullOrWhiteSpace($BaseToken) -and
    -not [string]::IsNullOrWhiteSpace($TableId) -and
    -not [string]::IsNullOrWhiteSpace($RecordId)) {
    $recordResponse = Invoke-FeishuJson -Method Get `
        -Uri "https://open.feishu.cn/open-apis/bitable/v1/apps/$([uri]::EscapeDataString($BaseToken))/tables/$([uri]::EscapeDataString($TableId))/records/$([uri]::EscapeDataString($RecordId))" `
        -Headers $headers `
        -Body $null
    $result["bitable_record"] = $recordResponse.data.record
}

$result | ConvertTo-Json -Depth 30
