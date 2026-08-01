[CmdletBinding()]
param(
    [switch]$CheckOnly,
    [switch]$SkipFrontendInstall,
    [switch]$UpgradeLegacyConfig
)

$ErrorActionPreference = "Stop"
$projectRoot = Split-Path -Parent $PSScriptRoot
$configPath = Join-Path $projectRoot "config.toml"
$configExamplePath = Join-Path $projectRoot "config.example.toml"
$composePath = Join-Path $projectRoot "compose.yaml"
$databaseInitPath = Join-Path $projectRoot "docker/mysql/init/001-create-databases.sql"
$configUpgradePath = Join-Path $PSScriptRoot "upgrade_local_config.py"

if ($CheckOnly -and $UpgradeLegacyConfig) {
    throw "-CheckOnly 不能与 -UpgradeLegacyConfig 同时使用。"
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

function Get-LocalConfigInspection {
    $inspectionOutput = @(
        & python $configUpgradePath inspect `
            --config $configPath `
            --template $configExamplePath
    )
    Assert-LastExitCode "检查本地 config.toml 版本失败。"
    try {
        return ($inspectionOutput -join "`n") | ConvertFrom-Json
    } catch {
        throw "本地 config.toml 检查器输出格式无效。"
    }
}

foreach ($command in @("rustup", "cargo", "docker", "node", "corepack", "python")) {
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
        $config = Get-Content -Raw -LiteralPath $configExamplePath
        if (
            -not $config.Contains($mysqlPlaceholder) -or
            -not $config.Contains($tokenPlaceholder)
        ) {
            throw "config.example.toml 缺少预期占位值，无法安全生成本机配置。"
        }

        $tokenBytes = [byte[]]::new(48)
        [Security.Cryptography.RandomNumberGenerator]::Fill($tokenBytes)
        $tokenSecret = [Convert]::ToBase64String($tokenBytes)
        $config = $config.Replace(
            $mysqlPlaceholder,
            "mysql://root:yang-local@127.0.0.1:3306/yang_system"
        )
        $config = $config.Replace($tokenPlaceholder, $tokenSecret)
        Set-Content -LiteralPath $configPath -Value $config -Encoding utf8NoBOM
        Write-Host "已生成本机 config.toml。"
    } else {
        $inspection = Get-LocalConfigInspection
        if (-not $inspection.current) {
            if (-not $UpgradeLegacyConfig) {
                throw "已有 config.toml 与当前启动契约不兼容。请备份后使用 -UpgradeLegacyConfig 显式升级。"
            }

            $upgradeArguments = @(
                $configUpgradePath,
                "upgrade",
                "--config",
                $configPath,
                "--template",
                $configExamplePath
            )
            $upgradeOutput = @(& python @upgradeArguments)
            Assert-LastExitCode "升级旧版 config.toml 失败。"
            $inspection = Get-LocalConfigInspection
            if (-not $inspection.current) {
                throw "升级后的 config.toml 仍不满足当前启动契约。"
            }
            Write-Host "旧版 config.toml 已升级到当前本地启动契约。"
            $backupLine = $upgradeOutput |
                Where-Object { $_.StartsWith("backup=") } |
                Select-Object -First 1
            if ($null -ne $backupLine) {
                Write-Host "旧配置备份: $($backupLine.Substring("backup=".Length))"
            }
        } else {
            Write-Host "保留已有 config.toml。"
        }
    }

    if (-not $SkipFrontendInstall) {
        pnpm --dir frontend install --frozen-lockfile
        Assert-LastExitCode "前端依赖安装失败。"
    }
} finally {
    Pop-Location
}

Write-Host "本地环境初始化完成。"
Write-Host "后端启动会先预检旧数据并同步 Schema: Set-Location project/yang-system; cargo run --locked"
Write-Host "前端: Set-Location project/yang-system; pnpm --dir frontend dev"
