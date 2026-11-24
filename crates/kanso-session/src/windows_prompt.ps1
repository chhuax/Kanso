$global:__KansoOriginalPrompt = $function:prompt
function global:prompt {
    $text = & $global:__KansoOriginalPrompt
    $location = $executionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem') {
        [Console]::Write(('{0}]9;9;{1}{2}' -f [char]27, $location.ProviderPath, [char]7))
    }
    $text
}

# 将默认目录蓝底改为蓝色粗体字，保留用户自定义的目录样式。
if ($PSVersionTable.PSVersion -ge [version]'7.2' -and
    $PSStyle.FileInfo.Directory -ceq "$([char]27)[44;1m") {
    $PSStyle.FileInfo.Directory = $PSStyle.Foreground.Blue + $PSStyle.Bold
}
