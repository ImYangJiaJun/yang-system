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
#   ⚠️ 但**先看这一条**：本脚本的 -EdgeBindAddr 默认是 0.0.0.0，含义是
#      「把应用边缘发布到**所有网卡**」，也就是**明文 http 直接暴露到公网**。
#      本仓库当前的部署目标（测试环境）就是这么用的；如果你的环境不需要公网直连，
#      请显式传 `-EdgeBindAddr 127.0.0.1`，否则你会在不知情的情况下开放一个
#      明文入口——登录凭据与密码重置令牌都会在公网上明文传输。
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
# 两个附加动作开关（默认都不做；可以单独用，也可以一起开）：
#   .\deploy.ps1 -SyncConfig              # 只做一件事：把本机 deploy/config.cloud.toml
#                                         #   传到服务器、就地替换那份配置，并重启应用容器
#   .\deploy.ps1 -ResetDb                 # 只做一件事：清空服务器上本系统数据库的全部数据
#                                         #   （DROP + 重建空库）并清空 Redis，由应用启动时重建 schema
#   .\deploy.ps1 -SyncConfig -ResetDb     # 两件一起：重置配置 + 重置数据库
#   .\deploy.ps1 -Mode deploy -SyncConfig -ResetDb
#                                         # 一条龙：先部署新镜像，**再**重置。
#                                         #   顺序不能反：库要清在**新版本已经在跑**之后，
#                                         #   否则旧容器一被 --restart 拉起就会把旧 schema
#                                         #   同步进刚清空的库（破坏性 schema 变更就白做了）。
#   ⚠️ 不给 -Mode 时这两个开关**不会**触发构建（刷新配置/清库是运维动作，不该顺带重打镜像，
#      构建要 8~9 分钟）。它们按「先做 -Mode 的远程步骤，再做这两件事」的次序执行。
#   ⚠️ 两个开关都是**不可逆**的（覆盖服务器凭据 / DROP DATABASE）。默认会打印影响面并要求
#      你输入 yes；不想被问就加 -Yes（脚本化用），但**没有确认就不会执行**：无法交互时直接
#      失败，绝不因为「没人回答」而放行。
#   ⚠️ 与 -Mode logs / upload 组合会直接报错：logs 是交互式阻塞命令（Ctrl+C 之后紧接着清库
#      太危险），upload 本就不触发远程（重置后重启的仍是旧镜像，语义容易误解）。
#   ⚠️ -ResetDb 还额外**只认 -Mode deploy**：别的 Mode 跑完时线上仍是旧版本，重启起来的是旧
#      容器，它会用旧 schema 同步进刚清空的库（smoke 尤其容易踩：它只起绿容器）。
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

    # ---------- 两个附加动作（默认都不开；详见文件头用法）----------
    #
    # -SyncConfig：把本机 deploy/config.cloud.toml 上传到服务器，**就地替换**服务器上那份
    #   配置，然后重启应用容器让它生效——配置只在进程启动时读一次，光换文件是不生效的。
    #   服务器上原来那份会先备份为 config.cloud.toml.bak.<时间戳>；重启后健康检查不通过时
    #   会自动把备份还原回去并再次重启（不会把服务器留在「配置坏了又没人知道」的状态）。
    #   ⚠️ 它会**覆盖服务器上的生产凭据**。本脚本此前的约定是「只上传模板，绝不上传本机的
    #      config.cloud.toml」（见上传段的注释）——那条约定是为了防止本机（可能是过期的、
    #      占位值的）配置盖掉服务器上填好的值。所以这个能力必须显式 opt-in，绝不是默认行为。
    #   ⚠️ 推送**改不动**这三项：http.bind / observability.metrics_enabled /
    #      observability.metrics_bind —— docker/app/Dockerfile 里同名 ENV 优先级更高。
    #   ⚠️ 改 token.active_secret 会让所有已签发 Token 失效；改 security.totp.aead_key 会让
    #      已绑定 TOTP 的用户无法登录；改 feishu.encryption_key 会让库里已封存的密文解不开。
    #      这三项在确认提示里会单独点出来。
    [switch]$SyncConfig,

    # -ResetDb：清空服务器上**本系统数据库容器里**的全部数据（DROP DATABASE + 重建空库），
    #   并一并清空 Redis（会话 / 授权版本缓存 / 验证码 / step-up 令牌）。
    #   「本系统数据库容器」= 服务器 config.cloud.toml 里 [mysql].url 指向的那个库；MySQL
    #   容器本身、数据卷、root 密码都保留。清完之后应用启动时会重建全部表（schema 同步只在
    #   启动时跑一次）——这正是「schema 同步永不删列」背景下应用破坏性 schema 变更的唯一途径。
    #   ⚠️ **不可逆**：MySQL 没有备份机制，清掉就没了。默认要求交互确认（见 -Yes）。
    #   ⚠️ 清库之后**没有任何账号能登录**：注册出来的新账号零权限，而授予权限只有手工 SQL
    #      （见 docs/contracts/AUTHZ_GRANTS.md），系统没有「首个注册账号即管理员」的引导。
    [switch]$ResetDb,

    # 跳过上面两个开关的交互确认（脚本化 / 无人值守用）。
    # 不给它时：脚本会打印影响面并要求输入 yes 才继续；**无法交互时直接失败**，
    # 不会因为「没人回答」而放行。
    [switch]$Yes,

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

    # 应用边缘（宿主端口）的发布绑定地址。
    #   **本文件默认 0.0.0.0** —— 这台部署目标（测试环境）就是要用「公网 IP:18654」直接访问，
    #   所以默认值取公网。改成 127.0.0.1 会让线上容器只绑 loopback、公网立刻不可达。
    #   ⚠️ 0.0.0.0 是**明文 http 直连公网**：凭据 / 会话 Cookie / 密码重置令牌都会以明文传输。
    #   ⚠️ **同事注意**：「拿本脚本部署 = 默认把应用暴露到公网」。若你的环境不需要公网直连，
    #      务必显式传 `-EdgeBindAddr 127.0.0.1`，并把边缘交给宿主机上的受信 TLS 代理。
    #   注意 deploy-blue-green.sh 里 BIND_ADDR 的默认值**仍是 127.0.0.1**（被 CI 部署合同
    #   门禁 frontend/scripts/verify-deployment-contract.mjs 锁定，不要改）——本参数的作用
    #   就是每次显式覆盖它，所以这里的默认值才是实际生效的那个。
    #   管理面 9154 不受本参数影响，始终只绑 loopback（见 deploy-blue-green.sh 的
    #   METRICS_BIND_ADDR），公网不可见。
    [string]$EdgeBindAddr = '0.0.0.0',

    # 应用边缘的宿主端口（线上）。
    # 留空 = 用 deploy-blue-green.sh 里的默认值（当前 18654）——**刻意让默认值只有
    # 一处事实源**，避免两个文件各写一个数字、日后漂移。
    # 需要临时改端口时在这里传，例如 -LiveHostPort 18655。
    [string]$LiveHostPort = '',

    # logs 模式要跟踪的容器名（默认空 = 线上后端）
    [string]$LogContainer = ''
)
$ErrorActionPreference = 'Stop'

# 是否**显式**指定了 -Mode。$Mode 自带默认值 'deploy'，所以只看它的值分不清「用户写了
# -Mode deploy」与「用户什么都没写」——而这两者对新开关的含义不同：没写 -Mode 时，
# -SyncConfig / -ResetDb 是**纯运维动作**，不该顺带触发 8~9 分钟的构建。
$ModeExplicit = $PSBoundParameters.ContainsKey('Mode')

# 破坏性动作的交互确认是否已通过。只有它为真，才把 RESET_DB_CONFIRM=yes 转发给远端——
# 于是「没问过」与「用户不同意」都拿不到那个值，远端会据此拒绝执行。
$ResetConfirmed = $false

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

# ---------- 两个开关的用法约束（在动手之前就判掉）----------
$postRequested = $SyncConfig -or $ResetDb
if ($postRequested -and $ModeExplicit -and $Mode -in @('logs','upload')) {
    # 拒绝而不是「忽略开关」：静默忽略正是本脚本历史上反复踩的那类坑
    # （参数写了却什么也没发生，输出还一切正常）。
    throw "-Mode $Mode 不能和 -SyncConfig / -ResetDb 组合：`n" +
          "      · logs   —— 远程命令是交互式阻塞的（Ctrl+C 退出），紧接着执行清库太危险；`n" +
          "      · upload —— 它本就不触发远程，重置后重启的仍是旧镜像，语义容易误解。`n" +
          "      请去掉 -Mode（只做这两件事），或显式写 -Mode deploy（先部署新镜像再重置）。"
}
if ($ResetDb -and $ModeExplicit -and $Mode -ne 'deploy') {
    # -ResetDb 的前提是「库清在**新版本已经在跑**之后」：否则被重启的是旧容器，它会用旧 schema
    # 同步进刚清空的库，破坏性 schema 变更静默失效——这正是 upload 被拒的同一条理由。
    # smoke 尤其容易踩：它只起绿容器，线上那对仍是旧镜像。
    throw "-ResetDb 只能与 -Mode deploy 组合，或干脆不给 -Mode（只重置、不部署）。`n" +
          "      -Mode $Mode 跑完时线上运行的还不是新版本，接着重置并重启起来的会是**旧容器**，`n" +
          "      它会用旧 schema 同步进刚清空的库——你想用清库来落地的破坏性 schema 变更会静默失效。`n" +
          "      部署新镜像再重置：  .\deploy.ps1 -Mode deploy -ResetDb`n" +
          "      只重置数据库：      .\deploy.ps1 -ResetDb"
}

# SSH/SCP 公共健壮性选项。实测踩过：TCP 已建立、认证也成功，但命令请求丢失，
# 两端互等数分钟（客户端 ssh 进程 CPU≈0；服务器上会话已建、却没有任何子进程）。
# 这组选项把这种「静默黑洞」变成**有界失败**，而不是让整个部署无限挂起。
$SshOpts = @('-o', 'ConnectTimeout=15', '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=4')

function Exec([scriptblock]$block) {
    & $block
    if ($LASTEXITCODE -ne 0) { throw "命令失败（退出码 $LASTEXITCODE）" }
}

# 构造远端 deploy-blue-green.sh 需要的环境变量前缀。
#
# ⚠️ **本脚本每个影响部署的参数都必须在这里转发**。漏转发的后果不是「报错」而是
#    **静默不生效**：远端取自己的默认值，本地传的参数形同虚设。最坏的一种是
#    `-BackendImage foo:v2` —— 脚本仍跑旧镜像，还会把 foo:v2 当「未使用」prune 掉，
#    最后打印「部署完成 ✔」。（2026-09-23 审计发现：BACKEND_IMAGE / FRONTEND_IMAGE /
#    BACKEND_TAR / APP_DIR 四个当时全都没转发，而 -RemoteDir 只用在 `cd` 上——
#    远端脚本自己还会 `cd "$APP_DIR"` 一次，不转发就会落到默认目录。）
#    对照表：-RemoteDir→APP_DIR、-BackendImage→BACKEND_IMAGE、-FrontendImage→
#    FRONTEND_IMAGE、-TarFile→BACKEND_TAR、-EdgeBindAddr→BIND_ADDR、-LiveHostPort→LIVE_HOST_PORT、
#    -SyncConfig→SYNC_CONFIG、-ResetDb→RESET_DB（+确认通过后才有 RESET_DB_CONFIRM=yes）。
#    远端认的其余变量（NET / CONFIG_FILE / 各容器名 / GREEN_* / METRICS_* / STAGED_CONFIG）
#    本脚本没有对应参数，需要时请在服务器上直接调 deploy-blue-green.sh。
#    （清库的「逃生门」RESET_DB_ALLOW_NONLOCAL 曾经存在、现已删除：它只会让 SQL 打在本机容器上
#     却报成功。现在 [mysql].url / [redis].url 的主机名不是本容器时一律拒绝。）
function Get-RemoteEnv {
    $pairs = [ordered]@{
        'APP_DIR'        = $RemoteDir
        'BACKEND_IMAGE'  = $BackendImage
        'FRONTEND_IMAGE' = $FrontendImage
        'BACKEND_TAR'    = $TarFile
        'BIND_ADDR'      = $EdgeBindAddr
    }
    # 留空 = 不传，让 deploy-blue-green.sh 的默认值生效（默认值只有一处事实源）。
    if ($LiveHostPort) { $pairs['LIVE_HOST_PORT'] = $LiveHostPort }
    # refresh 子命令的动作开关：只有远端的 cmd_refresh 会读它们，其余子命令直接忽略。
    # RESET_DB_CONFIRM 是远端**必填**的确认闸门，只在本地交互确认通过后才转发——
    # 少了它远端会拒绝执行清库（fail-closed），这正是我们要的。
    if ($SyncConfig) { $pairs['SYNC_CONFIG'] = '1' }
    if ($ResetDb) {
        $pairs['RESET_DB'] = '1'
        if ($ResetConfirmed) { $pairs['RESET_DB_CONFIRM'] = 'yes' }
    }
    ($pairs.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' '
}

function Invoke-Remote([string]$remoteCommand) {
    # BIND_ADDR 只影响真正发布容器的子命令（smoke / cutover / deploy），其余子命令忽略它。
    # 用环境变量前缀传进去，**刻意不改动 deploy-blue-green.sh 里那个被 CI 部署合同门禁
    # 锁定的 loopback 默认值**（见 frontend/scripts/verify-deployment-contract.mjs）。
    # ⚠️ $remoteCommand 由调用方拼好，**已经含脚本名**（如 './deploy-blue-green.sh deploy'）。
    #    这里绝不能再拼一次 ./$DeployScript——那会变成
    #    `./deploy-blue-green.sh ./deploy-blue-green.sh deploy`，main() 按 $1 派发时
    #    收到 './deploy-blue-green.sh' 落入 *) 分支，所有 Mode 都会打 usage 后 exit 1。
    Exec { ssh -n @SshOpts -i $SshKey $sshTarget "cd $RemoteDir && chmod +x $DeployScript && $(Get-RemoteEnv) $remoteCommand" }
}

# =============================================================================
# 附加动作：-SyncConfig（上传并热替换配置） / -ResetDb（清空数据库 + Redis）
#
# 分工：**本脚本只负责「确认 + 传文件」**，真正动服务器状态的是远端的 refresh 子命令
# （见 deploy-blue-green.sh 的 cmd_refresh）。这样「库怎么清、配置怎么装、失败怎么回滚」
# 只有一处实现，不会在本机/远端各写一份然后漂移。
# =============================================================================

# 从 TOML 里取一个标量。只认 `key = "value"` 这一种形态，与远端 toml_get 同口径：
# 本机这里要做的只有「取出 [mysql].url」，不值得为它引一个 TOML 解析器。
function Get-ConfigScalar([string[]]$lines, [string]$section, [string]$key) {
    $cur = ''
    foreach ($line in $lines) {
        $t = "$line".Trim()
        if ($t -match '^\[(.+?)\]') { $cur = $matches[1]; continue }
        if ($cur -ne $section) { continue }
        if ($t -match ('^' + [regex]::Escape($key) + '\s*=\s*(.+)$')) {
            return ($matches[1] -replace '\s+#.*$', '').Trim().Trim('"')
        }
    }
    return ''
}

# 把配置摊成「段.键 → 原值」的表。值**只用于比较**：下面的差异预览只打印键名，
# 一个值都不许出现在输出里（这是含生产凭据的文件）。
function Get-ConfigKeyMap([string[]]$lines) {
    $map = [ordered]@{}
    $cur = ''
    foreach ($line in $lines) {
        $t = "$line".Trim()
        if ($t -match '^\[(.+?)\]') { $cur = $matches[1]; continue }
        if (-not $t -or $t.StartsWith('#')) { continue }
        if ($t -match '^([A-Za-z0-9_.\-]+)\s*=\s*(.+)$') {
            $k = $matches[1]
            if ($cur) { $k = "$cur.$k" }
            $map[$k] = ($matches[2] -replace '\s+#.*$', '').Trim()
        }
    }
    return $map
}

# 会「让存量状态失效」或「改了也不生效」的键：确认提示里单独点出来。
# 前三条来自代码而非猜测：token.active_secret 是 JWT 签名密钥（config/mod.rs build_manager），
# security.totp.aead_key 加密 users.totp_secret（account/domain/mfa.rs），
# feishu.encryption_key 封存 DB 里的 token_cipher（feishu/domain/crypto.rs）。
# 末尾三条是被镜像 ENV 压过的键（docker/app/Dockerfile 明确写了优先级）。
$ImpactfulConfigKeys = [ordered]@{
    'token.active_secret'           = '改它 = 所有已签发 Token 立即失效（所有人要重新登录）'
    'security.totp.aead_key'        = '它加密库里的 users.totp_secret：改它 = 已绑定 TOTP 的用户登录不了'
    'feishu.encryption_key'         = '它封存库里的飞书 token 密文：改它 = 那些密文解不开'
    'mysql.url'                     = '改账号/密码不会自动生效（只在数据卷为空时初始化）；改库名 = 连带换库'
    'redis.url'                     = '换 Redis = 会话与授权缓存全丢（所有人要重新登录）'
    'app.environment'               = 'production 强制 https 与 metrics；test 会关掉这些强制'
    'http.bind'                     = '⚠️ 被镜像 ENV 压过：改这里不生效'
    'observability.metrics_enabled' = '⚠️ 被镜像 ENV 压过：改这里不生效'
    'observability.metrics_bind'    = '⚠️ 被镜像 ENV 压过：改这里不生效'
}

# 服务器上有没有 config.cloud.toml。**用退出码判定，不要解析 stdout**——ssh 会带出
# MOTD/banner，stdout 不保证干净；用 stdout 判定会把「服务器已有配置」误判成「没有」。
# （上传段里那段既有逻辑踩过同一个坑，这里沿用同一条规矩。）
function Test-RemoteConfigPresent {
    & ssh -n @SshOpts -i $SshKey $sshTarget "test -f $RemoteDir/config.cloud.toml"
    switch ($LASTEXITCODE) {
        0 { return $true }
        1 { return $false }
        default { throw "无法确认服务器上 config.cloud.toml 的状态（ssh 退出码 $LASTEXITCODE），中止以免误判。" }
    }
}

function Get-RemoteConfigLines {
    # ⚠️ 必须临时把控制台输出编码设成 UTF-8：PowerShell 5.1 会用 [Console]::OutputEncoding
    #    （中文 Windows 上是 GBK/936）去解码**本机进程**的 stdout。配置里有中文注释，
    #    按 GBK 解 UTF-8 字节会让变长编码的尾字节吃掉后面的换行 → 行结构被打乱 → 差异预览漏报。
    $prev = [Console]::OutputEncoding
    try {
        [Console]::OutputEncoding = [System.Text.Encoding]::UTF8
        $lines = & ssh -n @SshOpts -i $SshKey $sshTarget "cat $RemoteDir/config.cloud.toml"
    }
    finally { [Console]::OutputEncoding = $prev }
    if ($LASTEXITCODE -ne 0) { throw "读取服务器上的 config.cloud.toml 失败（ssh 退出码 $LASTEXITCODE）。" }
    return @($lines)
}

# 打印「服务器现在那份 → 本机这份」的**键级**差异。只出现键名，绝不出现值。
function Show-ConfigChangePreview([string[]]$remoteLines, [string[]]$localLines) {
    $server = Get-ConfigKeyMap $remoteLines
    $local  = Get-ConfigKeyMap $localLines
    $added   = @($local.Keys  | Where-Object { -not $server.Contains($_) })
    $removed = @($server.Keys | Where-Object { -not $local.Contains($_) })
    $changed = @($local.Keys  | Where-Object { $server.Contains($_) -and ($server[$_] -ne $local[$_]) })

    if (-not ($added.Count -or $removed.Count -or $changed.Count)) {
        Write-Host "    与服务器上现有配置逐键相同（推上去只是重放一遍）。" -ForegroundColor DarkGray
        return
    }
    Write-Host ("    将发生变化：修改 {0} 项 / 删除 {1} 项 / 新增 {2} 项" -f $changed.Count, $removed.Count, $added.Count)
    foreach ($k in $changed) {
        $note = ''
        if ($ImpactfulConfigKeys.Contains($k)) { $note = "   <= $($ImpactfulConfigKeys[$k])" }
        Write-Host "      ~ ${k}$note" -ForegroundColor Yellow
    }
    foreach ($k in $removed) {
        Write-Host "      - ${k}（本机这份里没有：推送后它会从服务器配置里消失；若是必填项，应用会启动失败）" -ForegroundColor Yellow
    }
    # 新增的键：既可能是「服务器那份还是旧模板」，也可能**直接让应用起不来**——配置结构体都带
    # deny_unknown_fields，多写一个键就在反序列化阶段失败。所以不能只报个数（以前就是这么写的，
    # 把最该看的风险藏起来了）。太多时只列前 12 个。
    $shown = @($added | Select-Object -First 12)
    foreach ($k in $shown) {
        Write-Host "      + ${k}（服务器那份里没有；多写的键会让应用启动失败，确认这是本应用认识的键）" -ForegroundColor Yellow
    }
    if ($added.Count -gt $shown.Count) {
        Write-Host ("      + …另有 {0} 个新键（未逐条列出）" -f ($added.Count - $shown.Count))
    }
}

# 破坏性动作的确认闸门。默认要求手工输入 yes；-Yes 跳过；**无法交互时直接失败**。
function Confirm-PostSteps {
    Write-Host "`n==> 破坏性操作确认" -ForegroundColor Yellow
    Write-Host "    服务器      : $sshTarget"
    if ($ResetDb) {
        Write-Host "    清空范围    : 服务器 config.cloud.toml 里 [mysql].url 指向的**整个库**（DROP DATABASE）"
        Write-Host "                  同时清空 Redis（会话 / 授权版本缓存 / 验证码 / step-up 令牌）"
        Write-Host "    保留        : MySQL 容器、数据卷、root 密码、同机其它库"
        Write-Host "    不可恢复    : MySQL 没有备份机制，库内数据删除后找不回来" -ForegroundColor Red
        Write-Host "    清库之后    : 没有任何账号能登录——新注册的账号零权限，授权只能手工 SQL" -ForegroundColor Red
        Write-Host "                  （docs/contracts/AUTHZ_GRANTS.md）" -ForegroundColor Red
        Write-Host "    旧 Token    : 清库会删掉 users 行，请求期的授权版本校验因此会拒绝**绝大多数**旧" -ForegroundColor Yellow
        Write-Host "                  Token（缓存未命中/版本不等时会回查 MySQL，用户不存在的直接拒）。但重建后" -ForegroundColor Yellow
        Write-Host "                  自增 id 从 1 重排：若旧 Token 的 authz_version 恰好等于新账号的版本" -ForegroundColor Yellow
        Write-Host "                  （如都是默认 1），存在被继承的可能。要彻底作废：改 [token].active_secret" -ForegroundColor Yellow
        Write-Host "                  后再 -SyncConfig（那会让所有人重新登录）。" -ForegroundColor Yellow
    }
    if ($SyncConfig) {
        $local = Get-LocalCloudConfigPath
        Write-Host "    覆盖配置    : $local"
        Write-Host "                  → 服务器 $RemoteDir/config.cloud.toml（原文件先备份）"
        if (Test-RemoteConfigPresent) {
            # 本机这份也必须按 UTF-8 读，理由见 Assert-LocalCloudConfigReady 里的说明。
            Show-ConfigChangePreview (Get-RemoteConfigLines) (Get-Content -LiteralPath $local -Encoding UTF8)
        } else {
            Write-Host "    服务器上还没有 config.cloud.toml：本次会新建一份。" -ForegroundColor Yellow
        }
    }
    if ($Yes) {
        Write-Host "    -Yes：已跳过交互确认。" -ForegroundColor Yellow
        $script:ResetConfirmed = $true
        return
    }
    if ([Console]::IsInputRedirected) {
        # 关键：不能因为「没人回答」就当成同意。
        throw "当前会话没有可交互的输入，无法完成确认。请在有终端的会话里运行，或显式加 -Yes。"
    }
    $answer = Read-Host "    继续请输入 yes（其它任何输入都会中止）"
    if ($answer -ne 'yes') { throw "已取消：没有收到 yes。" }
    $script:ResetConfirmed = $true
}

function Get-LocalCloudConfigPath {
    Join-Path $scriptDir 'config.cloud.toml'
}

# 把部署产物（脚本 + compose + 模板）同步到服务器。**只传这三个，绝不传本机 config.cloud.toml**
# （那个走 Send-LocalConfig 的暂存路径）。
# 纯运维路径（不给 -Mode 的 -SyncConfig / -ResetDb）也**必须**走这一步：远端 refresh 是**新版**
# 脚本里才有的子命令，服务器上那份还是旧的时，main() 会打进 usage 后 exit 1，本地只看到一句
# 「命令失败（退出码 1）」——完全指不出方向（2026-09-24 复审发现）。构建/上传路径本来就会同步。
function Send-DeployArtifacts {
    $deployScriptPath = Join-Path $scriptDir $DeployScript
    # 部署脚本是硬依赖：缺了它远端根本没法执行，所以这里**失败关闭**，不像下面两个可选文件那样跳过。
    if (-not (Test-Path $deployScriptPath)) { throw "本机缺少部署脚本：$deployScriptPath" }
    Exec { scp @SshOpts -i $SshKey $deployScriptPath "${sshTarget}:${RemoteDir}/" }
    Write-Host "    已同步 $DeployScript"
    foreach ($extra in @('compose.infra.yaml', 'config.cloud.example.toml')) {
        $local = Join-Path $scriptDir $extra
        if (Test-Path $local) {
            Exec { scp @SshOpts -i $SshKey $local "${sshTarget}:${RemoteDir}/" }
            Write-Host "    已同步 $extra"
        }
    }
}

# 本机配置的**只读**预检：文件在不在 / 占位值 / 缺 [authorization] 段 / [mysql].url 写法。
# 与远端 require_config 同口径，但跑在本机——传上去再被远端拒绝等于白跑一趟，还会在服务器上
# 留一个含明文凭据的暂存文件。
# 单独拆出来是为了能在**构建之前**也判一遍：本机这份没填好的话，`-Mode deploy -SyncConfig`
# 以前要等完 8~9 分钟构建 + 上传 + 远端部署成功，最后才抛「先填完再上传」（2026-09-24 复审发现）。
function Assert-LocalCloudConfigReady {
    $local = Get-LocalCloudConfigPath
    if (-not (Test-Path $local)) { throw "-SyncConfig 需要本机配置，但找不到：$local" }
    # ⚠️ 必须显式 -Encoding UTF8。配置是无 BOM 的 UTF-8（模板就是），而 PowerShell 5.1 的
    #    Get-Content 在无 BOM 时按 **ANSI/GBK** 解码：GBK 是变长编码，UTF-8 中文注释的尾字节
    #    会**吃掉后面的换行**，于是 `url = "..."` 被粘进上一行注释、整行以 # 开头，下面的键匹配
    #    就落空——2026-09-24 实测报「读不到 [mysql].url」而配置其实完全正确（服务器端的 awk 逐字节
    #    读，不受影响，所以只有本机预检会误判）。
    $lines = Get-Content -LiteralPath $local -Encoding UTF8

    $hits = Select-String -Path $local -Pattern '=\s*"[^"]*(CHANGE_ME|replace-with)' -Encoding UTF8 |
            Where-Object { $_.Line -notmatch '^\s*#' }
    if ($hits) {
        Write-Host "    本机配置里仍有未填写的占位值：" -ForegroundColor Red
        $hits | ForEach-Object { Write-Host ("      {0}: {1}" -f $_.LineNumber, $_.Line.Trim()) -ForegroundColor Red }
        throw "先填完再上传：$local"
    }
    if (-not ($lines -match '^\s*\[authorization\]')) { throw "$local 看起来不完整（缺 [authorization] 段）。" }
    $url = Get-ConfigScalar $lines 'mysql' 'url'
    if (-not $url) { throw "$local 里读不到 [mysql].url。" }
    if ($url -notmatch '^mysql://') { throw "[mysql].url 必须以 mysql:// 开头，实际读到：$url" }
    # 服务器上应当连的是**容器名**（如 yang-mysql）。指向 loopback 基本可以断定是拿错了文件
    # （本机开发配置）——那会让服务器去连它自己的 3306，而那里并没有 MySQL。
    $dbHost = ($url -split '@')[-1].Split('/')[0].Split(':')[0]
    if ($dbHost -match '^(localhost|127\.0\.0\.1|host\.docker\.internal)$') {
        Write-Host "    ⚠️ [mysql].url 的主机名是 $dbHost —— 看着像**本机开发配置**；服务器上应当填容器名（如 yang-mysql）。" -ForegroundColor Yellow
    }
    return $lines
}

# 上传本机配置到服务器的**暂存路径**，由远端 refresh 校验后就地安装。
# （就地安装的必要性见远端 install_staged_config：配置是单文件 bind mount，换 inode 的写法
#   运行中的容器读不到新内容。）
function Send-LocalConfig {
    $local = Get-LocalCloudConfigPath
    Assert-LocalCloudConfigReady | Out-Null   # 与构建前那道预检同一份实现，重复调用代价可忽略
    Exec { scp @SshOpts -i $SshKey $local "${sshTarget}:${RemoteDir}/config.cloud.toml.upload" }
    Write-Host "    已上传到 $RemoteDir/config.cloud.toml.upload（远端校验后就地安装）" -ForegroundColor Green
}

# 执行两个附加动作。两个开关都没开时是**空操作**，所以它可以无脑挂在每条远程路径的末尾。
function Invoke-PostSteps {
    if (-not $postRequested) { return }
    if ($ResetDb -and -not $ResetConfirmed) {
        # 正常流程到不了这里（确认在构建/远程之前就做完了）。真到了，说明调用顺序被改坏了——
        # 宁可报错，也绝不「没确认就把库清了」。
        throw "内部错误：-ResetDb 尚未确认就进入了执行阶段（调用顺序被改动了？）。"
    }
    $what = @()
    if ($SyncConfig) { $what += '上传并热替换配置' }
    if ($ResetDb)    { $what += '清空数据库与 Redis' }
    Write-Host "`n==> 附加动作：$($what -join ' + ')" -ForegroundColor Cyan
    if ($SyncConfig) { Send-LocalConfig }
    Invoke-Remote "./$DeployScript refresh"
    # 措辞刻意中性：远端在「没有线上容器可探测」时会 warn + exit 0（它什么都没验证），
    # 本地不能替它宣称「已验证」——那正是远端刚修掉的那类「把未验证当成功」。
    Write-Host "    远端 refresh 已执行完毕；**是否验证通过见上面的输出**（配置改坏时它会自动回滚配置）。" -ForegroundColor Green
}

# ---------- 首次部署的提前拒绝（必须放在构建之前）----------
# -SyncConfig 与一次部署是同一次运行里的两步，而配置是在部署**之后**才安装的（顺序不能反，见
# 文件头）。所以服务器上还没有配置时，马上要跑的远端 deploy/smoke 会卡在 require_config 上——
# 在这里就判掉，别让人等完 8~9 分钟的构建才撞见它（这段以前放在上传段，被 2026-09-24 复审
# 指出与它自己的注释矛盾）。
if ($SyncConfig -and $ModeExplicit -and $Mode -in @('deploy','smoke') -and -not (Test-RemoteConfigPresent)) {
    throw "服务器上还没有 config.cloud.toml，而 -Mode $Mode 一上来就要用它（远端会先做配置校验）。`n" +
          "      先单独把配置推上去，再回来部署：`n" +
          "        .\deploy.ps1 -SyncConfig     # 只推配置，不构建`n" +
          "        .\deploy.ps1                 # 再正常部署`n" +
          "      或者去掉 -SyncConfig，让脚本按老路子传模板（然后你登录服务器填）。"
}

# 本机那份配置也在这里先判一遍（纯只读，不联网）：不然 `-Mode deploy -SyncConfig` 会在等完
# 8~9 分钟构建、上传、远端部署成功之后才因为「本机配置没填」而失败，留下「部署成了、配置没推」
# 的半完成态，重跑还要再付一次构建。
if ($SyncConfig) { Assert-LocalCloudConfigReady | Out-Null }

# ---------- 破坏性动作的确认（在构建/远程**之前**做）----------
# 放在这里而不是「执行前一刻」：否则 -Mode deploy 的用户要在等完 8~9 分钟构建之后才被问，
# 而那时人很可能已经走开了。确认只做一次，动作仍在流程末尾执行。
# 构建或远程步骤失败时脚本会中止（$ErrorActionPreference='Stop' + Invoke-Remote 抛错），
# 于是**部署没成功就不会去清库**——这一点是有意为之。
if ($postRequested) { Confirm-PostSteps }

# ---------- 仅远程操作，不打包 ----------
if ($Mode -in @('cutover','rollback','status','infra')) {
    $sub = if ($Mode -eq 'infra') { 'infra-up' } else { $Mode }
    Invoke-Remote "./$DeployScript $sub"
    # 这条路径不打包、也不经过上传段，但它照样会执行远端 refresh —— 那是**新脚本**才有的子命令，
    # 服务器上那份还是旧的时只会打 usage 并 exit 1，而暂存的本机配置（含明文凭据、scp 默认权限）
    # 会留在服务器上（远端 chmod/删除都在 refresh 里，不会执行）。所以这里也要先同步。
    if ($postRequested) { Send-DeployArtifacts }
    Invoke-PostSteps
    exit 0
}

# ---------- 追踪容器实时日志（-t 强制伪终端，保证 -f 流式输出与 Ctrl+C 正常）----------
if ($Mode -eq 'logs') {
    $logArg = if ($LogContainer) { " $LogContainer" } else { '' }
    Write-Host "`n==> 追踪容器实时日志（Ctrl+C 退出）" -ForegroundColor Cyan
    & ssh @SshOpts -t -i $SshKey $sshTarget "cd $RemoteDir && chmod +x $DeployScript && ./$DeployScript logs$logArg"
    exit 0
}

# ---------- 只做附加动作，不打包、不传镜像 ----------
# 没显式给 -Mode 时走这里：刷新配置 / 清库是运维动作，不该顺带重打镜像（构建 8~9 分钟）。
# 想「部署新镜像 + 重置」就显式写 -Mode deploy ——那种组合是**先部署、后重置**，理由见文件头。
if ($postRequested -and -not $ModeExplicit) {
    # 先同步部署产物（尤其 deploy-blue-green.sh）：远端 refresh 是**新脚本**才有的子命令，
    # 服务器上那份还是旧的时只会打 usage 并 exit 1，本地看不出原因。
    Write-Host "`n==> 同步部署脚本到 ${sshTarget}:${RemoteDir}" -ForegroundColor Cyan
    Send-DeployArtifacts
    Invoke-PostSteps
    Write-Host "`n完成（未构建、未传镜像：这两个动作都不涉及镜像）。" -ForegroundColor Green
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
# 部署脚本 + compose + 模板（**不含**本机的 config.cloud.toml）。与纯运维路径共用同一个实现，
# 免得两处清单各写一份然后漂移。
Send-DeployArtifacts
# **默认只上传模板，绝不上传本机的 config.cloud.toml。**
# 生产凭据只应存在于服务器上：本机那份含明文密钥，一旦上传就会覆盖服务器上已填好的值。
# 想主动推送本机那份，用显式的 -SyncConfig（下面这段会被跳过，改由 refresh 安装并先备份）。
if ($SyncConfig) {
    # -SyncConfig 与本次部署是同一次运行里的两步，而配置是在部署**之后**才安装的（顺序不能反，
    # 见文件头）。服务器上还没有配置这件事已经在构建之前判掉了（见上面的「首次部署的提前拒绝」）。
    Write-Host "    -SyncConfig：跳过模板引导，稍后由远端的 refresh 安装本机配置（原文件会先备份）。" -ForegroundColor Yellow
}
else {
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
            Write-Host "       （或改成本机填好一份，用 -SyncConfig 推上去）" -ForegroundColor Yellow
            exit 1
        }
        default {
            throw "无法确认服务器上 config.cloud.toml 的状态（ssh 退出码 $LASTEXITCODE），中止以免覆盖凭据。"
        }
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
    # ⚠️ 这条提示必须带上与脚本自身**相同**的环境变量前缀。以前它是裸的
    #    `./deploy-blue-green.sh smoke`，照抄就会落到 .sh 的默认绑定（loopback）上——
    #    「脚本自己跑」与「照提示跑」得到不同的发布结果（2026-09-23 审计发现）。
    Write-Host "  服务器上：cd $RemoteDir && chmod +x $DeployScript && $(Get-RemoteEnv) ./$DeployScript smoke"
    exit 0
}
Write-Host "`n==> [5/5] 远程执行 $Mode" -ForegroundColor Cyan
Invoke-Remote "./$DeployScript $Mode"
# 附加动作在**部署成功之后**才执行。Invoke-Remote 失败会抛错终止整个脚本，所以
# 「部署失败 → 仍然清了库」这种情况不会发生。
Invoke-PostSteps
Write-Host "`n完成。" -ForegroundColor Green
