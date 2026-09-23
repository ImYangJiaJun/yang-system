# =============================================================================
# yang-system 本地部署脚本（Windows）：打包两个镜像 → 上传服务器 → 触发蓝绿部署
# 依赖：Docker Desktop（Linux 容器模式）、Windows OpenSSH
#
# ⚠️ 本文件必须保存为 **UTF-8 with BOM**。Windows PowerShell 5.1 对无 BOM 的文件
#    按 ANSI 解析，会把下面的中文注释解成乱码并**直接破坏字符串引号配对**，报
#    「The string is missing the terminator」。用会抹掉 BOM 的编辑器改完请确认：
#        head -c 3 deploy.ps1 | od -An -tx1   # 应为 ef bb bf
#    （本仓库的脚本约定跑在 pwsh 7 上，但本脚本刻意兼容 5.1，因为部署机不一定装了 7。）
#
# 【同事上手：只需改下面 param 里的 3 个 SSH 默认值】
#   把 $SshUser / $SshHost / $SshKey 改成你自己的，然后直接运行即可。
#
# 用法：
#   .\deploy.ps1                  # 完整：build + save + 上传 + 远程 smoke + cutover
#   .\deploy.ps1 -Mode upload     # 只 build + 上传，不触发远程
#   .\deploy.ps1 -Mode smoke      # build + 上传 + 远程冒烟（不切流量）
#   .\deploy.ps1 -Mode cutover    # 仅远程切流量（不打包）
#   .\deploy.ps1 -Mode rollback   # 仅远程回退（不打包）
#   .\deploy.ps1 -Mode status     # 仅查看远程状态（不打包）
#   .\deploy.ps1 -Mode infra      # 仅远程启动 MySQL + Redis（首次部署时用）
#   .\deploy.ps1 -Mode logs       # 追踪线上后端容器日志（Ctrl+C 退出）
#   .\deploy.ps1 -Mode logs -LogContainer yang-backend-green   # 追踪绿容器
#   .\deploy.ps1 -SkipBuild       # 跳过构建+导出，复用已有镜像包直接上传（上传/远程失败后的重试）
#
# 注意构建上下文：
#   后端 = lib_yang 仓库根（因为 project/yang-system 通过相对路径依赖 ../../crates）
#   前端 = project/yang-system/frontend
#   本脚本按相对位置自动推算，不需要手工指定。
# =============================================================================
[CmdletBinding()]
param(
    [ValidateSet('deploy','upload','smoke','cutover','rollback','status','infra','logs')]
    [string]$Mode = 'deploy',

    # 跳过 [1/5]~[3/5]（构建 + 导出），复用已存在的 $TarFile 直接上传。
    # 用于「构建已成功、但上传或远程步骤失败」后的重试：构建要 8~9 分钟，不该重付。
    [switch]$SkipBuild,

    # ↓↓↓ 部署时只需改这三个（用户名 / IP / 私钥路径）↓↓↓
    [string]$SshUser = 'yjj',
    [string]$SshHost = '47.109.148.207',
    [string]$SshKey  = 'C:\Users\16040\.ssh\yjj_47.109.148.207',
    # ↑↑↑ 改到这里为止 ↑↑↑

    [string]$RemoteDir = '/home/yjj/yang-system',
    [string]$BackendImage  = 'yang-system-backend:live',
    [string]$FrontendImage = 'yang-system-frontend:live',
    [string]$TarFile = 'yang-system-images.tar.gz',
    [string]$DeployScript = 'deploy-blue-green.sh',

    # 应用边缘（宿主 8154）的发布绑定地址。
    #   127.0.0.1（默认）= 只对宿主机可见，公网访问交给宿主机上的受信 TLS 边缘。
    #   0.0.0.0          = 直接用「公网 IP:8154」访问。⚠️ 那是**明文 http 直连公网**，
    #                      凭据 / 会话 Cookie / 密码重置令牌都会以明文传输，仅适合临时联调。
    # 注意：管理面 9154 **不受本参数影响**，始终只绑 loopback（见 deploy-blue-green.sh）；
    #       否则把这里改成 0.0.0.0 会顺手把 /metrics 一起挂到公网上。
    [string]$EdgeBindAddr = '127.0.0.1',

    # 应用边缘的宿主端口（线上）。
    # 留空 = 用 deploy-blue-green.sh 里的默认值（当前 18654）——**刻意让默认值只有
    # 一处事实源**，避免两个文件各写一个数字、日后漂移。
    # 需要临时改端口时在这里传，例如 -LiveHostPort 18655。
    [string]$LiveHostPort = '',

    # logs 模式要跟踪的容器名（默认空 = 线上后端）
    [string]$LogContainer = ''
)
$ErrorActionPreference = 'Stop'

# ---------- 路径推算 ----------
# 本文件在 <libyang>/project/yang-system/deploy/，所以仓库根是它的上三级。
$scriptDir = $PSScriptRoot
$projectDir = Split-Path -Parent $scriptDir          # .../project/yang-system
$libYangRoot = Split-Path -Parent (Split-Path -Parent $projectDir)   # .../<libyang>
$frontendDir = Join-Path $projectDir 'frontend'
# 相对**仓库根**的路径（不是相对构建上下文）。下面用 Join-Path 拼成绝对路径再传给 -f，
# 原因见构建那一步的注释：docker 按进程 CWD 解析 -f。
$backendDockerfile = 'project/yang-system/docker/app/Dockerfile'

Set-Location $scriptDir

if (-not (Test-Path (Join-Path $libYangRoot 'Cargo.toml'))) {
    throw "推算出的 lib_yang 仓库根不含 Cargo.toml：'$libYangRoot'。请确认本脚本仍位于 project/yang-system/deploy/ 下。"
}
if (-not (Test-Path (Join-Path $frontendDir 'pnpm-lock.yaml'))) {
    throw "找不到前端构建上下文：'$frontendDir'（缺 pnpm-lock.yaml）。"
}

# 校验私钥路径
if (-not $SshKey -or -not (Test-Path $SshKey)) {
    throw "私钥不存在：'$SshKey'。请在 deploy.ps1 里把 -SshKey 改成你自己的私钥路径。"
}
$sshTarget = "${SshUser}@${SshHost}"

# SSH/SCP 公共健壮性选项。实测踩过：TCP 已建立、认证也成功，但命令请求丢失，
# 两端互等数分钟（客户端 ssh 进程 CPU≈0；服务器上会话已建、却没有任何子进程）。
# 这组选项把这种「静默黑洞」变成**有界失败**，而不是让整个部署无限挂起。
$SshOpts = @('-o', 'ConnectTimeout=15', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=4')

function Exec([scriptblock]$block) {
    & $block
    if ($LASTEXITCODE -ne 0) { throw "命令失败（退出码 $LASTEXITCODE）" }
}

function Invoke-Remote([string]$remoteCommand) {
    # BIND_ADDR 只影响真正发布容器的子命令（smoke / cutover / deploy），其余子命令忽略它。
    # 用环境变量前缀传进去，**刻意不改动 deploy-blue-green.sh 里那个被 CI 部署合同门禁
    # 锁定的 loopback 默认值**（见 frontend/scripts/verify-deployment-contract.mjs）。
    # ⚠️ $remoteCommand 由调用方拼好，**已经含脚本名**（如 './deploy-blue-green.sh deploy'）。
    #    这里绝不能再拼一次 ./$DeployScript——那会变成
    #    `./deploy-blue-green.sh ./deploy-blue-green.sh deploy`，main() 按 $1 派发时
    #    收到 './deploy-blue-green.sh' 落入 *) 分支，所有 Mode 都会打 usage 后 exit 1。
    $envPrefix = "BIND_ADDR=$EdgeBindAddr"
    if ($LiveHostPort) { $envPrefix += " LIVE_HOST_PORT=$LiveHostPort" }
    Exec { ssh -n @SshOpts -i $SshKey $sshTarget "cd $RemoteDir && chmod +x $DeployScript && $envPrefix $remoteCommand" }
}

# ---------- 仅远程操作，不打包 ----------
if ($Mode -in @('cutover','rollback','status','infra')) {
    $sub = if ($Mode -eq 'infra') { 'infra-up' } else { $Mode }
    Invoke-Remote "./$DeployScript $sub"
    exit 0
}

# ---------- 追踪容器实时日志（-t 强制伪终端，保证 -f 流式输出与 Ctrl+C 正常）----------
if ($Mode -eq 'logs') {
    $logArg = if ($LogContainer) { " $LogContainer" } else { '' }
    Write-Host "`n==> 追踪容器实时日志（Ctrl+C 退出）" -ForegroundColor Cyan
    & ssh @SshOpts -t -i $SshKey $sshTarget "cd $RemoteDir && chmod +x $DeployScript && ./$DeployScript logs$logArg"
    exit 0
}

# ---------- 1. 本地构建 ----------
# ⚠️ -f 必须传**绝对路径**。docker CLI 是按**进程 CWD** 解析 -f 的，而本脚本已经
#    Set-Location 到 deploy/，所以相对写法会去找 deploy/project/yang-system/... 而失败
#    （实测报 "resolve : GetFileAttributesEx deploy: ... cannot find the file"）。
$tarGz = Join-Path $scriptDir $TarFile

if ($SkipBuild) {
    # 复用上一次导出的镜像包。典型场景：一次完整部署在**上传或远程步骤**失败，
    # 而改变的东西没有变——构建要 8~9 分钟（网络受限时更久），重试不该再付一遍。
    Write-Host "`n==> [1/5]~[3/5] 跳过构建与导出（-SkipBuild）" -ForegroundColor Yellow
    # 缺镜像包就**失败关闭**，绝不退化成「悄悄重新构建」：那样调用方以为跳过生效了，
    # 实际又等 9 分钟，还可能打进一份与预期不同的源码状态。
    if (-not (Test-Path $tarGz)) {
        $msg = "指定了 -SkipBuild，但找不到可复用的镜像包：'$tarGz'。`n" +
               "      注意：上一次**上传成功**后本脚本会删掉这个文件，那种情况下无法跳过构建。`n" +
               "      请去掉 -SkipBuild 重新构建；若只想重跑远程步骤，用 -Mode cutover / -Mode rollback。"
        throw $msg
    }
    $sizeMb = [math]::Round((Get-Item $tarGz).Length / 1MB, 1)
    $stamp  = (Get-Item $tarGz).LastWriteTime.ToString('yyyy-MM-dd HH:mm:ss')
    Write-Host "    复用 $TarFile（$sizeMb MB，导出于 $stamp）" -ForegroundColor Green
    Write-Host "    ⚠️ 该包可能已与当前工作区源码不一致，确认它就是你要发布的那一份。" -ForegroundColor Yellow
}
else {
    Write-Host "`n==> [1/5] 构建后端镜像 $BackendImage" -ForegroundColor Cyan
    Write-Host "    上下文：$libYangRoot（必须在仓库根，前端/后端都靠它解析 ../../crates）"
    Exec { docker build -f (Join-Path $libYangRoot $backendDockerfile) -t $BackendImage $libYangRoot }

    Write-Host "`n==> [2/5] 构建前端镜像 $FrontendImage" -ForegroundColor Cyan
    Write-Host "    上下文：$frontendDir"
    Exec { docker build -f (Join-Path $frontendDir 'deploy/Dockerfile') -t $FrontendImage $frontendDir }

    # ---------- 2. 导出并 gzip 压缩 ----------
    Write-Host "`n==> [3/5] 导出镜像并压缩" -ForegroundColor Cyan
    $tarRaw = Join-Path $scriptDir 'yang-system-images.tar'
    if (Test-Path $tarGz) { Remove-Item $tarGz }
    if (Test-Path $tarRaw) { Remove-Item $tarRaw }
    Exec { docker save -o $tarRaw $BackendImage $FrontendImage }
    $in  = [System.IO.File]::OpenRead($tarRaw)
    $out = [System.IO.File]::Create($tarGz)
    $gz  = [System.IO.Compression.GZipStream]::new($out, [System.IO.Compression.CompressionMode]::Compress)
    $in.CopyTo($gz)
    $gz.Dispose(); $in.Dispose(); $out.Dispose()
    Remove-Item $tarRaw
    $sizeMb = [math]::Round((Get-Item $tarGz).Length / 1MB, 1)
    Write-Host "    已生成 $TarFile（$sizeMb MB）" -ForegroundColor Green
}

# ---------- 3. 上传 ----------
Write-Host "`n==> [4/5] 上传到 ${sshTarget}:${RemoteDir}" -ForegroundColor Cyan
# ⚠️ 这里**刻意不预先** `ssh ... "mkdir -p $RemoteDir"`。实测该调用会静默挂起数分钟
#    （见上面对 $SshOpts 的说明），而且远端目录本来就应该先建好——参考实现
#    D:\code\ProfitScope\web\deploy.ps1 也是直接 scp，没有这一步。
#    代价是远端目录不存在时 scp 会失败，所以下面把失败翻译成可操作的提示。
& scp @SshOpts -i $SshKey $tarGz "${sshTarget}:${RemoteDir}/"
if ($LASTEXITCODE -ne 0) {
    throw "上传失败（退出码 $LASTEXITCODE）。`n" +
          "      若报 'No such file or directory'，说明服务器上还没有 $RemoteDir；`n" +
          "      请先登录服务器执行 mkdir -p $RemoteDir，再重跑本脚本`n" +
          "      （加 -SkipBuild 可复用已构建的镜像包，免去重新构建）。"
}
foreach ($extra in @($DeployScript, 'compose.infra.yaml', 'config.cloud.example.toml')) {
    $local = Join-Path $scriptDir $extra
    if (Test-Path $local) {
        Exec { scp @SshOpts -i $SshKey $local "${sshTarget}:${RemoteDir}/" }
        Write-Host "    已同步 $extra"
    }
}
# **只上传模板，绝不上传本机的 config.cloud.toml。**
# 生产凭据只应存在于服务器上：本机那份含明文密钥，一旦上传就会覆盖服务器上已填好的值，
# 而且它在版本库里（.gitignore 已忽略，但工作区里的明文仍然不该外流）。
# **用退出码判定，不要解析 stdout**：ssh 可能带出 MOTD/banner，stdout 不保证是干净的
# `yes`/`no`；用 stdout 判定会导致「服务器已有配置」被误判成「没有」，进而用模板
# **覆盖掉服务器上已填好的凭据**。
& ssh -n @SshOpts -i $SshKey $sshTarget "test -f $RemoteDir/config.cloud.toml"
switch ($LASTEXITCODE) {
    0 {
        Write-Host "    服务器上已有 config.cloud.toml，跳过（不覆盖你的凭据）" -ForegroundColor Green
    }
    1 {
        Exec { ssh -n @SshOpts -i $SshKey $sshTarget "cp $RemoteDir/config.cloud.example.toml $RemoteDir/config.cloud.toml" }
        Write-Host "    服务器上还没有 config.cloud.toml，已从模板复制一份。" -ForegroundColor Yellow
        Write-Host "    ⚠️ 请登录服务器填写里面的所有 replace-with-*，然后重跑本脚本。" -ForegroundColor Yellow
        Write-Host "       （凭据只填在服务器上；本机不要保留生产口令）" -ForegroundColor Yellow
        exit 1
    }
    default {
        throw "无法确认服务器上 config.cloud.toml 的状态（ssh 退出码 $LASTEXITCODE），中止以免覆盖凭据。"
    }
}

# 上传完成后删除本地导出的离线镜像文件（镜像已在服务器上，本地不再需要）
if (Test-Path $tarGz) {
    Remove-Item $tarGz
    Write-Host "    已删除本地离线镜像 $TarFile" -ForegroundColor Green
}

# ---------- 4. 触发远程 ----------
if ($Mode -eq 'upload') {
    Write-Host "`n上传完成（未触发远程）。可手动执行：" -ForegroundColor Green
    Write-Host "  服务器上：cd $RemoteDir && chmod +x $DeployScript && ./$DeployScript smoke"
    exit 0
}
Write-Host "`n==> [5/5] 远程执行 $Mode" -ForegroundColor Cyan
Invoke-Remote "./$DeployScript $Mode"
Write-Host "`n完成。" -ForegroundColor Green
