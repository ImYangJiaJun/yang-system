[CmdletBinding()]
param(
    [switch]$CheckOnly,
    [switch]$SkipFrontendInstall,
    [switch]$RunMigrations
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $projectRoot "config.toml"
$configExamplePath = Join-Path $projectRoot "config.example.toml"
$composePath = Join-Path $projectRoot "compose.yaml"
$databaseInitPath = Join-Path $projectRoot "docker/mysql/init/001-create-databases.sql"

if ($CheckOnly -and $RunMigrations) {
    throw "-CheckOnly 与 -RunMigrations 不能同时使用。"
}

function Assert-Command {
    param([Parameter(Mandatory)][string]$Name)

    if (-not (Get-Command $Name -ErrorAction SilentlyContinue)) {
        throw "缺少必需命令: $Name"
    }
}

function Assert-LastExitCode {
    param([Parameter(Mandatory)][string]$Message)

    if ($LASTEXITCODE -ne 0) {
        throw $Message
    }
}

foreach ($command in @("rustup", "cargo", "docker", "node", "corepack")) {
    Assert-Command $command
}

$nodeVersion = (& node --version).Trim()
Assert-LastExitCode "无法读取 Node.js 版本。"
$nodeMajorText = $nodeVersion.TrimStart([char]"v").Split(".")[0]
$nodeMajor = 0
if (-not [int]::TryParse($nodeMajorText, [ref]$nodeMajor) -or $nodeMajor -lt 24) {
    throw "Node.js 版本必须为 24 或更高，当前版本: $nodeVersion"
}

docker info *> $null
Assert-LastExitCode "Docker Engine 未运行，请先启动 Docker Desktop。"
docker compose version *> $null
Assert-LastExitCode "Docker Compose 不可用。"

if ($CheckOnly) {
    rustup run 1.97.1 rustc --version *> $null
    Assert-LastExitCode "Rust 1.97.1 未安装，请运行不带 -CheckOnly 的初始化命令。"
    Write-Host "本地环境必需工具检查通过。"
    exit 0
}

rustup toolchain install 1.97.1 --profile minimal --component rustfmt,clippy
Assert-LastExitCode "Rust 1.97.1 工具链安装失败。"

Push-Location $projectRoot
try {
    corepack enable
    Assert-LastExitCode "Corepack 启用失败。"
    corepack install --global pnpm@10.33.1
    Assert-LastExitCode "pnpm 10.33.1 安装失败。"

    docker compose -f $composePath up -d --wait
    Assert-LastExitCode "MySQL 或 Redis 启动失败。"
    Get-Content -Raw -LiteralPath $databaseInitPath |
        docker compose -f $composePath exec -T mysql sh -c 'mysql -uroot -p"$MYSQL_ROOT_PASSWORD"'
    Assert-LastExitCode "本地 MySQL 数据库初始化失败。"

    if (-not (Test-Path -LiteralPath $configPath)) {
        $mysqlPlaceholder = "mysql://root:password@127.0.0.1:3306/yang_system"
        $tokenPlaceholder = "replace-with-at-least-32-random-bytes"
        $bootstrapPlaceholder = "replace-with-yang-bootstrap-secret-digest"
        $schemaValidate = 'mode = "validate"'
        $config = Get-Content -Raw -LiteralPath $configExamplePath
        if (
            -not $config.Contains($mysqlPlaceholder) -or
            -not $config.Contains($tokenPlaceholder) -or
            -not $config.Contains($bootstrapPlaceholder) -or
            -not $config.Contains($schemaValidate)
        ) {
            throw "config.example.toml 缺少预期占位值，无法安全生成本机配置。"
        }

        $tokenBytes = [byte[]]::new(48)
        [Security.Cryptography.RandomNumberGenerator]::Fill($tokenBytes)
        $tokenSecret = [Convert]::ToBase64String($tokenBytes)
        $bootstrapOutput = @(& cargo run --quiet --locked --bin yang-bootstrap-secret)
        Assert-LastExitCode "生成本地 bootstrap secret 失败。"
        $bootstrapSecretLine = $bootstrapOutput |
            Where-Object { $_.StartsWith("secret=") } |
            Select-Object -First 1
        $bootstrapDigestLine = $bootstrapOutput |
            Where-Object { $_.StartsWith("digest=") } |
            Select-Object -First 1
        if ($null -eq $bootstrapSecretLine -or $null -eq $bootstrapDigestLine) {
            throw "bootstrap secret 生成器输出格式无效。"
        }
        $bootstrapSecret = $bootstrapSecretLine.Substring("secret=".Length)
        $bootstrapDigest = $bootstrapDigestLine.Substring("digest=".Length)
        $config = $config.Replace(
            $mysqlPlaceholder,
            "mysql://root:yang-local@127.0.0.1:3306/yang_system"
        )
        $config = $config.Replace($tokenPlaceholder, $tokenSecret)
        $config = $config.Replace($bootstrapPlaceholder, $bootstrapDigest)
        Set-Content -LiteralPath $configPath -Value $config -Encoding utf8NoBOM
        Write-Host "已生成本机 config.toml（schema.mode=validate）。"
        Write-Host "本地 bootstrap secret（仅显示一次，请立即保存）: $bootstrapSecret"
    } else {
        Write-Host "保留已有 config.toml。"
        $existingConfig = Get-Content -Raw -LiteralPath $configPath
        if (
            -not $existingConfig.Contains("[bootstrap]") -or
            -not $existingConfig.Contains("secret_digest")
        ) {
            throw "已有 config.toml 缺少 [bootstrap].secret_digest；请运行 yang-bootstrap-secret 并手工加入摘要。"
        }
    }

    if (-not $SkipFrontendInstall) {
        pnpm --dir frontend install --frozen-lockfile
        Assert-LastExitCode "前端依赖安装失败。"
    }
    if ($RunMigrations) {
        cargo run --locked --bin yang-migrate -- apply --config $configPath
        Assert-LastExitCode "本地版本化迁移失败。"
    }
} finally {
    Pop-Location
}

Write-Host "本地环境初始化完成。"
if ($RunMigrations) {
    Write-Host "版本化迁移与 Schema 校验已完成。后端: Set-Location project/yang-system; cargo run --locked"
} else {
    Write-Host "后端启动前提: 数据库 Schema 已对齐；首次建表或升级请先运行 yang-migrate apply。"
    Write-Host "迁移: Set-Location project/yang-system; cargo run --locked --bin yang-migrate -- apply"
    Write-Host "后端: Set-Location project/yang-system; cargo run --locked"
}
Write-Host "前端: Set-Location project/yang-system; pnpm --dir frontend dev"
