function global:__KansoGitBranch([string]$directory) {
    try {
        while ($directory) {
            $gitDir = [IO.Path]::Combine($directory, '.git')
            if ([IO.Directory]::Exists($gitDir)) { break }
            if ([IO.File]::Exists($gitDir)) {
                $pointer = Get-Content -LiteralPath $gitDir -Encoding UTF8 -TotalCount 1 -ErrorAction Stop
                if ($pointer -cnotmatch '^gitdir: (.+)$') { return }
                $gitDir = [IO.Path]::Combine($directory, $Matches[1].TrimEnd())
                break
            }
            $directory = [IO.Path]::GetDirectoryName($directory)
        }
        if (!$directory) { return }

        $head = (Get-Content -LiteralPath ([IO.Path]::Combine($gitDir, 'HEAD')) -Encoding UTF8 -TotalCount 1 -ErrorAction Stop).TrimEnd()
        if ($head.StartsWith('ref: ', [StringComparison]::Ordinal)) {
            return $head.Substring(5) -creplace '^refs/heads/', '' -replace '\p{Cc}', ''
        }
        if ($head -match '^(?:[0-9a-f]{40}|[0-9a-f]{64})$') {
            return $head.Substring(0, 7)
        }
    } catch {
        return
    }
}

function global:__KansoGitStatus([string]$directory, [string]$branch) {
    $lastExitCode = $global:LASTEXITCODE
    $PSNativeCommandUseErrorActionPreference = $false
    try {
        $records = & git.exe --no-optional-locks -c core.fsmonitor=false -C $directory status --porcelain=v2 --branch --ahead-behind --untracked-files=normal 2>$null
        if (!$?) { return $branch }
    } catch {
        return $branch
    } finally {
        $global:LASTEXITCODE = $lastExitCode
    }

    $ahead = $behind = $staged = $unstaged = $untracked = $conflicts = 0
    foreach ($record in $records) {
        switch -Regex ($record) {
            '^# branch\.ab \+(\d+) -(\d+)$' {
                $ahead = [int]$Matches[1]
                $behind = [int]$Matches[2]
            }
            '^[12] ([^ ])([^ ]) ' {
                if ($Matches[1] -ne '.') { $staged++ }
                if ($Matches[2] -ne '.') { $unstaged++ }
            }
            '^\? ' { $untracked++ }
            '^u ' { $conflicts++ }
        }
    }
    $summary = $branch
    if ($ahead) { $summary += " $([char]0x2191)$ahead" }
    if ($behind) { $summary += " $([char]0x2193)$behind" }
    if ($staged) { $summary += " +$staged" }
    if ($unstaged) { $summary += " ~$unstaged" }
    if ($untracked) { $summary += " ?$untracked" }
    if ($conflicts) { $summary += " !$conflicts" }
    $summary
}

$global:__KansoOriginalPrompt = $function:prompt
function global:prompt {
    $succeeded = $?
    $text = & $global:__KansoOriginalPrompt
    $location = $executionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem') {
        [Console]::Write(('{0}]9;9;{1}{2}' -f [char]27, $location.ProviderPath, [char]7))
        $ending = '>' * ($nestedPromptLevel + 1) + ' '
        if ($text -is [string] -and $text -ceq "PS $location$ending") {
            $branch = __KansoGitBranch $location.ProviderPath
            if (!$global:__KansoColorPrompt) {
                if ($branch) { return "PS $location [$branch]$ending" }
                return $text
            }
            $directory = $location.Path
            if ($directory -eq $HOME) {
                $directory = '~'
            } elseif ($directory.StartsWith("$HOME\", [StringComparison]::OrdinalIgnoreCase)) {
                $directory = '~' + $directory.Substring($HOME.Length)
            }
            if ($directory.Length -gt 38) {
                $parts = $directory -split '\\'
                $directory = '...\' + ($parts[-2..-1] -join '\')
            }
            $directory = $directory -replace '\p{Cc}', ''

            $esc = [char]27
            $arrow = [char]0xe0b0
            $folder = [char]0xf07b
            $git = [char]0xe0a0
            $statusColor = '42'
            $statusArrow = '32'
            $result = [char]0x2713
            if (!$succeeded) {
                $statusColor = '41'
                $statusArrow = '31'
                $result = '!'
            }

            $text = "$esc[30;46m $folder $directory "
            $previousColor = '36'
            if ($branch) {
                $summary = __KansoGitStatus $location.ProviderPath $branch
                $text += "$esc[36;43m$arrow$esc[30;43m $git $summary "
                $previousColor = '33'
            }
            $text += "$esc[$previousColor;${statusColor}m$arrow$esc[30;${statusColor}m $result $esc[49;${statusArrow}m$arrow$esc[0m $ending"
        }
    }
    $text
}

# 将默认目录蓝底改为蓝色粗体字，保留用户自定义的目录样式。
if ($PSVersionTable.PSVersion -ge [version]'7.2' -and
    $PSStyle.FileInfo.Directory -ceq "$([char]27)[44;1m") {
    $PSStyle.FileInfo.Directory = $PSStyle.Foreground.Blue + $PSStyle.Bold
}
