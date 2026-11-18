$global:__KansoOriginalPrompt = $function:prompt
function global:prompt {
    $text = & $global:__KansoOriginalPrompt
    $location = $executionContext.SessionState.Path.CurrentLocation
    if ($location.Provider.Name -eq 'FileSystem') {
        [Console]::Write(('{0}]9;9;{1}{2}' -f [char]27, $location.ProviderPath, [char]7))
    }
    $text
}
