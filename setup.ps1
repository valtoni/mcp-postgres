#Requires -Version 5.1
<#
.SYNOPSIS
  Configura mcp-dba-postgres em Claude Desktop, Codex CLI e Gemini CLI.

.DESCRIPTION
  - Verifica Docker e baixa a imagem do Docker Hub se nao existir local.
  - Atualiza os arquivos de config dos clientes MCP, preservando outras
    entradas. Cria backup .bak.<timestamp> antes de qualquer modificacao.
  - Opcionalmente cria um databases.yaml de template em -YamlTemplateAt.

.PARAMETER Image
  Tag da imagem Docker. Default: valtoni/mcp-dba-postgres:1.1

.PARAMETER Tools
  Lista de clientes a configurar: claude, codex, gemini (default: todos).

.PARAMETER YamlTemplateAt
  Caminho de diretorio onde criar um databases.yaml de exemplo.

.PARAMETER Yes
  Aceita sobrescrever entradas 'mcp-dba-postgres' ja existentes sem perguntar.

.EXAMPLE
  .\setup.ps1
  .\setup.ps1 -Tools claude,gemini -Yes
  .\setup.ps1 -YamlTemplateAt C:\projetos\meu-app
#>
[CmdletBinding()]
param(
    [string]$Image = "valtoni/mcp-dba-postgres:1.1",
    [ValidateSet("claude", "codex", "gemini")]
    [string[]]$Tools = @("claude", "codex", "gemini"),
    [string]$YamlTemplateAt,
    [switch]$Yes
)

$ErrorActionPreference = "Stop"
$ServerName = "mcp-dba-postgres"

function Write-Section([string]$msg) {
    Write-Host ""
    Write-Host "==> $msg" -ForegroundColor Cyan
}

function Write-Ok([string]$msg)   { Write-Host "  OK    $msg" -ForegroundColor Green }
function Write-Skip([string]$msg) { Write-Host "  SKIP  $msg" -ForegroundColor Yellow }
function Write-Info([string]$msg) { Write-Host "  ..    $msg" }

function Confirm-Action([string]$message) {
    if ($Yes) { return $true }
    $r = Read-Host "$message [y/N]"
    return $r -match '^[yY]'
}

function Test-DockerAvailable {
    try {
        $null = & docker version --format '{{.Server.Version}}' 2>$null
        return ($LASTEXITCODE -eq 0)
    } catch { return $false }
}

function Test-LocalImage([string]$image) {
    $null = & docker image inspect $image 2>$null
    return ($LASTEXITCODE -eq 0)
}

function Backup-File([string]$path) {
    if (Test-Path -LiteralPath $path) {
        $stamp  = Get-Date -Format 'yyyyMMdd-HHmmss'
        $backup = "$path.bak.$stamp"
        Copy-Item -LiteralPath $path -Destination $backup
        Write-Info "backup -> $backup"
    }
}

function Write-TextUtf8NoBom([string]$path, [string]$content) {
    $dir = Split-Path -Path $path -Parent
    if ($dir -and -not (Test-Path -LiteralPath $dir)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }
    $utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($path, $content, $utf8NoBom)
}

function Get-DockerRunCommandLine([string]$image) {
    $cmd = 'docker run -i --rm -e PGPASSWORD -v %cd%:/project -w /project'
    if ($env:KUBECONFIG) {
        $kpath = $env:KUBECONFIG.Replace('\', '/')
        $cmd += " -e KUBECONFIG=/kubeconfig -v " + $kpath + ":/kubeconfig:ro"
    } else {
        $cmd += ' -v %USERPROFILE%\.kube:/root/.kube:ro'
    }
    $cmd += ' -v //./pipe/docker_engine://./pipe/docker_engine ' + $image
    return $cmd
}

function New-JsonMcpEntry([string]$image) {
    $cmdLine = Get-DockerRunCommandLine $image
    return [pscustomobject]@{
        command = "cmd.exe"
        args    = @("/c", $cmdLine)
    }
}

function Update-JsonMcpConfig([string]$path, [string]$image, [string]$label) {
    Write-Section "Configurando $label"
    Write-Info "arquivo: $path"

    $config = $null
    if (Test-Path -LiteralPath $path) {
        Backup-File $path
        try {
            $config = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
        } catch {
            throw "Falha lendo JSON em $path : $($_.Exception.Message)"
        }
    } else {
        $config = [pscustomobject]@{}
    }

    if (-not $config.PSObject.Properties.Match('mcpServers').Count) {
        $config | Add-Member -MemberType NoteProperty -Name 'mcpServers' -Value ([pscustomobject]@{})
    }

    $entry = New-JsonMcpEntry $image
    $exists = $config.mcpServers.PSObject.Properties.Match($ServerName).Count -gt 0

    if ($exists) {
        if (-not (Confirm-Action "  Entrada '$ServerName' ja existe em $label. Sobrescrever?")) {
            Write-Skip "mantendo entrada existente"
            return
        }
        $config.mcpServers.$ServerName = $entry
    } else {
        $config.mcpServers | Add-Member -MemberType NoteProperty -Name $ServerName -Value $entry
    }

    $json = $config | ConvertTo-Json -Depth 32
    Write-TextUtf8NoBom $path $json
    Write-Ok "gravado"
}

function Update-CodexTomlConfig([string]$path, [string]$image) {
    Write-Section "Configurando Codex CLI"
    Write-Info "arquivo: $path"

    $sectionHeader = "[mcp_servers.$ServerName]"
    $cmdLine       = Get-DockerRunCommandLine $image
    # No TOML: backslash dentro de string de aspas duplas precisa ser escapado.
    $cmdEscaped    = $cmdLine -replace '\\','\\'

    $block = @"
$sectionHeader
command = "cmd.exe"
args = ["/c", "$cmdEscaped"]
"@

    if (-not (Test-Path -LiteralPath $path)) {
        Write-TextUtf8NoBom $path ($block + "`r`n")
        Write-Ok "criado"
        return
    }

    Backup-File $path
    $content = Get-Content -LiteralPath $path -Raw -Encoding UTF8

    if ($content -match [regex]::Escape($sectionHeader)) {
        if (-not (Confirm-Action "  Entrada '$ServerName' ja existe em Codex. Sobrescrever?")) {
            Write-Skip "mantendo entrada existente"
            return
        }
        $pattern = "(?ms)" + [regex]::Escape($sectionHeader) + ".*?(?=^\[|\z)"
        $replacement = $block + "`r`n`r`n"
        $content = [regex]::Replace($content, $pattern, $replacement)
    } else {
        if (-not $content.EndsWith("`n")) { $content += "`r`n" }
        $content += "`r`n" + $block + "`r`n"
    }

    Write-TextUtf8NoBom $path $content
    Write-Ok "gravado"
}

function Maybe-CreateYamlTemplate([string]$dirPath) {
    if (-not $dirPath) { return }
    Write-Section "databases.yaml template"
    if (-not (Test-Path -LiteralPath $dirPath)) {
        Write-Skip "diretorio nao existe: $dirPath"
        return
    }
    $yamlPath = Join-Path $dirPath "databases.yaml"
    if (Test-Path -LiteralPath $yamlPath) {
        Write-Skip "ja existe: $yamlPath"
        return
    }
    $tpl = @"
version: 1
default: local
databases:
  - alias: local
    host: host.docker.internal
    port: 5432
    user: postgres
    database: postgres
    password_ref: "env://PGPASSWORD"
    source: static
    description: "Postgres do dev local"
"@
    Write-TextUtf8NoBom $yamlPath $tpl
    Write-Ok "criado: $yamlPath"
}

# ---------------------------- main ----------------------------

Write-Section "Pre-requisitos"
if (-not (Test-DockerAvailable)) {
    throw "Docker nao esta acessivel. Inicie o Docker Desktop e tente novamente."
}
Write-Ok "Docker respondendo"

Write-Section "Imagem Docker"
Write-Info "imagem: $Image"
if (Test-LocalImage $Image) {
    Write-Ok "ja esta local"
} else {
    Write-Info "nao encontrada local; baixando do Docker Hub..."
    & docker pull $Image
    if ($LASTEXITCODE -ne 0) {
        throw "Falha baixando $Image. Verifique acesso ao Docker Hub."
    }
    Write-Ok "imagem baixada"
}

foreach ($tool in $Tools) {
    switch ($tool) {
        'claude' { 
            Update-JsonMcpConfig "$env:APPDATA\Claude\claude_desktop_config.json" $Image "Claude Desktop"
            if (Test-Path -LiteralPath "$env:USERPROFILE\.claude.json") {
                Update-JsonMcpConfig "$env:USERPROFILE\.claude.json" $Image "Claude CLI (Claude Code)"
            }
        }
        'codex'  { Update-CodexTomlConfig "$env:USERPROFILE\.codex\config.toml" $Image }
        'gemini' { Update-JsonMcpConfig "$env:USERPROFILE\.gemini\settings.json" $Image "Gemini CLI" }
    }
}

Maybe-CreateYamlTemplate $YamlTemplateAt

Write-Host ""
Write-Host "Setup concluido." -ForegroundColor Green
Write-Host ""
Write-Host "Proximos passos:"
Write-Host "  1) Crie um databases.yaml na raiz do projeto que vai usar o MCP"
Write-Host "     (ou rode novamente: .\setup.ps1 -YamlTemplateAt <caminho>)."
Write-Host "  2) Exporte a variavel referenciada, ex.:"
Write-Host "       `$env:PGPASSWORD = 'sua-senha'"
Write-Host "  3) Reinicie Claude Desktop / Codex / Gemini para carregar a nova config."
