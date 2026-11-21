# Loaded only for an explicitly enabled, interactive local PowerShell profile.
# All changes are confined to this ConsoleHost process.
& {
    if ($Host.Name -ne 'ConsoleHost' -or $ExecutionContext.SessionState.LanguageMode -ne 'FullLanguage' -or
        (Test-Path Variable:global:__W11TermSemanticPrompt)) { return }

    $promptCommand = Get-Item Function:prompt -ErrorAction SilentlyContinue
    $readerCommand = Get-Item Function:PSConsoleHostReadLine -ErrorAction SilentlyContinue
    if (!$promptCommand -or !$readerCommand) { return }
    foreach ($command in $promptCommand, $readerCommand) {
        if (($command.Options -band 'ReadOnly, Constant') -or
            $command.Definition -match '\](?:133|633);[ABCD]') { return }
    }
    $global:__W11TermSemanticPrompt = @{
        Prompt = $promptCommand.ScriptBlock
        Reader = $readerCommand.ScriptBlock
        Prefix = [string][char]27 + ']133;'
        AwaitingInput = $false
        Executing = $false
        PreviousNativeCode = $null
    }

    function global:prompt {
        $private:succeeded = $?
        $private:state = $global:__W11TermSemanticPrompt
        # ConsoleHost evaluates the literal script "prompt" and invokes the
        # reader directly. PSReadLine redraws can invoke prompt while editing.
        if ($MyInvocation.Line -eq 'prompt' -and !$MyInvocation.ScriptName -and
            !$state.AwaitingInput -and !$NestedPromptLevel -and
            (!$state.Executing -or $MyInvocation.HistoryId -ne $state.BeforeHistoryId)) {
            if ($state.Executing) {
                $state.Executing = $false
                $private:code = 0
                if (!$succeeded) {
                    $private:native = $ExecutionContext.SessionState.PSVariable.GetValue('LASTEXITCODE')
                    # A cmdlet failure leaves the previous native code untouched.
                    # With no observable change we have only PowerShell's boolean.
                    $code = 1
                    if ($native -is [int] -and $native -ne 0 -and $native -ne $state.PreviousNativeCode) {
                        $code = $native
                    }
                }
                [Console]::Write($state.Prefix + 'D;' + $code.ToString([cultureinfo]::InvariantCulture) + [char]7)
            }
            [Console]::Write($state.Prefix + 'A' + [char]7)
            $state.AwaitingInput = $true
        }
        # Preserve the success bit visible to an existing status-aware prompt.
        # Ignore keeps the synthetic failure out of both stderr and $Error.
        if (!$succeeded) { Microsoft.PowerShell.Utility\Write-Error 'previous command failed' -ErrorAction Ignore }
        & $state.Prompt
    }

    function global:PSConsoleHostReadLine {
        $private:state = $global:__W11TermSemanticPrompt
        if (!$state.AwaitingInput -or $NestedPromptLevel -or $MyInvocation.ScriptLineNumber) {
            return (& $state.Reader)
        }
        [Console]::Write($state.Prefix + 'B' + [char]7)
        try {
            # Delegate editing, multiline input, history and all key handlers.
            # Only the shell receives the returned line, never the OSC payload.
            $private:line = & $state.Reader
        } finally {
            $state.AwaitingInput = $false
        }
        if ($line -is [string] -and ![string]::IsNullOrWhiteSpace($line)) {
            $state.PreviousNativeCode = $ExecutionContext.SessionState.PSVariable.GetValue('LASTEXITCODE')
            # A user explicitly calling "prompt" runs under this history ID;
            # ConsoleHost evaluates the real next prompt after recording it.
            $state.BeforeHistoryId = $MyInvocation.HistoryId
            $state.Executing = $true
            [Console]::Write($state.Prefix + 'C' + [char]7)
        }
        $line
    }
}
