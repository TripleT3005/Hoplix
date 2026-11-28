[CmdletBinding(SupportsShouldProcess, ConfirmImpact = 'High')]
param(
    [Parameter(Mandatory, Position = 0)]
    [ValidateSet('start', 'stop', 'env', 'status', 'rebuild', 'reset')]
    [string] $Command
)

# Settings

$image = 'hoplix-test-servers:latest'
$container = 'hoplix-test-servers'
$context = Join-Path $PSScriptRoot '..\tests\servers'
$stateDirectory = Join-Path $env:USERPROFILE '.hoplix-test-servers'
$environmentFile = Join-Path $stateDirectory 'test.env'
$ports = @('2222', '2223', '2224', '2221', '2990', '2280', '2443', '2900', '40000-40019')
$labels = @('project=hoplix', 'purpose=Hoplix integration test servers')

# Docker

function Invoke-Docker {
    & docker @args
    if ($LASTEXITCODE -ne 0) {
        throw "docker $($args[0]) failed with exit code $LASTEXITCODE"
    }
}

function Test-Image {
    [bool](docker image ls --quiet $image)
}

function Test-Container {
    [bool](docker ps --all --quiet --filter "name=^$container$")
}

function Test-Running {
    [bool](docker ps --quiet --filter "name=^$container$")
}

function Invoke-ImageBuild {
    Invoke-Docker build --pull --tag $image $context
}

function New-Container {
    New-Item -ItemType Directory -Force $stateDirectory | Out-Null
    $hostState = $stateDirectory -replace '\\', '/'
    $portArguments = $ports | ForEach-Object { '--publish'; "127.0.0.1:${_}:${_}" }
    $labelArguments = $labels | ForEach-Object { '--label'; $_ }
    Invoke-Docker create --name $container @labelArguments @portArguments `
        --volume "${hostState}:/state" --env "HOPLIX_HOST_STATE=$hostState" $image | Out-Null
}

function Wait-Healthy {
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        $health = docker inspect --format '{{.State.Health.Status}}' $container
        if ($health -eq 'healthy') {
            return
        }
        if (-not (Test-Running)) {
            throw "The container stopped; see: docker logs $container"
        }
        Start-Sleep -Milliseconds 500
    }
    throw "The servers did not become ready; see: docker logs $container"
}

# Environment

function Import-TestEnvironment {
    if (-not (Test-Path $environmentFile)) {
        throw "$environmentFile does not exist yet; run: test-servers.ps1 start"
    }
    $count = 0
    foreach ($line in Get-Content -Encoding UTF8 $environmentFile) {
        if ($line -match '^(HOPLIX_TEST_[A-Z0-9_]+)=(.*)$') {
            [Environment]::SetEnvironmentVariable($Matches[1], $Matches[2], 'Process')
            $count++
        }
    }
    Write-Host "Set $count HOPLIX_TEST_* variables in this session."
}

function Show-Status {
    $state = if (Test-Running) { 'running' } elseif (Test-Container) { 'stopped' } else { 'not created' }
    Write-Host "Container ${container}: $state"
    Write-Host "Credentials: $stateDirectory"
    Write-Host 'Ports on 127.0.0.1: SFTP/SCP 2222, keyboard-interactive 2223 (key + code) and 2224 (password + code),'
    Write-Host '  FTP/FTPES 2221, FTPS 2990, passive 40000-40019, WebDAV 2280 (HTTP) and 2443 (HTTPS), S3 2900'
}

# Commands

function Start-Servers {
    if (-not (Test-Image)) {
        Invoke-ImageBuild
    }
    if (-not (Test-Container)) {
        New-Container
    }
    if (-not (Test-Running)) {
        Invoke-Docker start $container | Out-Null
    }
    Wait-Healthy
    Show-Status
}

switch ($Command) {
    'start' {
        Start-Servers
    }
    'stop' {
        if (Test-Running) {
            Invoke-Docker stop $container | Out-Null
        }
        Show-Status
    }
    'env' {
        Import-TestEnvironment
    }
    'status' {
        Show-Status
    }
    'rebuild' {
        Invoke-ImageBuild
        if (Test-Container) {
            Invoke-Docker rm --force $container | Out-Null
        }
        Start-Servers
    }
    'reset' {
        if ($PSCmdlet.ShouldProcess("$container and $stateDirectory", 'Remove the container and all generated credentials')) {
            if (Test-Container) {
                Invoke-Docker rm --force $container | Out-Null
            }
            if (Test-Path $stateDirectory) {
                Get-ChildItem -Force $stateDirectory | Remove-Item -Recurse -Force
            }
            Start-Servers
        }
    }
}
