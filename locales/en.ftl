ui-shell-command = Command line
command-duration-milliseconds = { $value }ms
command-duration-seconds = { $value }s
command-duration-minutes = { $minutes }m { $seconds }s
command-duration-hours = { $hours }h { $minutes }m
## 说明：本文件为 core 消息的英文目录。key、参数与取值分支必须与 kanso-types 的
## CATALOG / AUX_KEYS 一致，由 `cargo xtask check-locales` 校验，规则见 docs/i18n.md。
## 参数 $endpoint 与 $reason 由平台先用 endpoint-* / reason-* 子 key 渲染成字符串再代入。

## 子 key：失败原因

git-failed = Git: { $detail }
git-summary-expired = The repository summary is out of date. Refresh to continue.
git-commit-message-invalid = The commit message must not be empty or exceed 16 KiB.
git-entry-unavailable = This entry is no longer available.
git-diff-unavailable = No text diff is available for binary files, non-regular files, or content exceeding 512 KiB.
git-diff-empty-file = New empty file. No text diff is available.
git-diff-binary = Binary file. No text diff is available.
git-diff-no-content-change = No net content changes relative to the current commit.
git-diff-metadata-only = Only metadata changed, or no text diff is available.
reason-not-found = not found
reason-permission-denied = permission denied
reason-already-exists = already exists
reason-timed-out = timed out
reason-connection-refused = connection refused
reason-connection-reset = connection reset
reason-host-unreachable = host unreachable
reason-name-resolution = name resolution failed
reason-other = { $detail }

## 子 key：连接对象

endpoint-direct = { $host_port }
endpoint-via = { $host_port } via { $via }

## Profile

profile-host-empty = Enter a host.
profile-user-empty = Enter a user name.
profile-locale-invalid = “{ $value }” is not a valid locale. Use only letters, digits and _ - . @, up to 64 characters.

## SSH

ssh-connect-failed = Can’t connect to { $endpoint }: { $reason }.
ssh-negotiation-failed = No common { $category ->
        [kex] key exchange algorithm
        [host-key] host key algorithm
        [cipher] encryption algorithm
        [mac] MAC algorithm
        [compression] compression algorithm
       *[other] algorithm
    } with { $endpoint }. The server offers: { $server_offers }.
ssh-keepalive-timeout = { $endpoint } stopped responding. The connection was closed.
ssh-connection-lost = The connection to { $endpoint } was lost.
ssh-shared-connection-closed = The connection to { $endpoint } is already closed, so this tab can’t be duplicated. Open a new session instead.
ssh-key-file-unreadable = Can’t read the private key file { $path }: { $reason }.
ssh-key-decrypt-failed = Can’t decrypt the private key { $path }. Check the key passphrase.
ssh-proxy-command-failed = The proxy command failed ({ $command }): { $reason }.

## Host key

hostkey-changed = The host key of { $endpoint } has changed. The new { $key_type } key fingerprint is { $fingerprint }. The connection was refused. Accept the new key only if you know this host was replaced.
hostkey-store-write-failed = Can’t save the host key to { $path }: { $reason }.

## Authentication

auth-partial-success = Your credentials were accepted by { $endpoint }, but user { $user } needs another factor. The server only offers: { $methods }.
auth-failed = { $method ->
        [password] Password authentication failed for { $user } on { $endpoint }.
        [private-key] Private key authentication failed for { $user } on { $endpoint }.
        [agent] Authentication failed for { $user } on { $endpoint }: no identity in the agent was accepted.
       *[other] Authentication failed for { $user } on { $endpoint }.
    }
auth-key-rejected = { $endpoint } rejected the private key { $path } for user { $user }.
auth-agent-no-identity = The ssh-agent has no usable identity for { $user } on { $endpoint }.
auth-agent-unavailable = Can’t reach the ssh-agent: { $details }.
auth-cancelled = Authentication to { $endpoint } was cancelled.
auth-too-many-rounds = { $endpoint } asked for more than { $max_rounds } rounds of verification. The connection was closed.
auth-response-rejected = { $endpoint } rejected the verification response.

## Jump hosts

jump-cycle = The jump host chain loops back to “{ $profile }”.
jump-missing = The jump host “{ $profile }” no longer exists.
jump-not-ssh = The jump host “{ $profile }” is not an SSH session.
jump-too-long = The jump host chain is too long. The maximum is { $max_hops ->
        [one] { $max_hops } hop
       *[other] { $max_hops } hops
    }.
jump-unreachable = Jump host { $jump } can’t reach { $target }: { $reason }.
jump-proxy-command-conflict = A jump host and a proxy command can’t be used together. Choose one.

## Session

session-exited = The session has ended.
session-exited-with-code = The session has ended (exit code { $code }).
session-connection-closed = The connection to { $endpoint } was closed.
session-spawn-failed = Can’t start { $command }: { $reason }.
session-not-terminal = This is an SFTP-only session and has no terminal.

## Current directory

cwd-no-shell = Couldn’t find the shell for this connection. Shells that report OSC 7 work on any system.
cwd-no-ssh-connection = The server doesn’t set SSH_CONNECTION. Shells that report OSC 7 work on any system.
cwd-other-user = The foreground process belongs to another user (for example sudo or su). Shells that report OSC 7 work on any system.
cwd-no-proc = The server has no /proc (only Linux is supported). Shells that report OSC 7 work on any system.
cwd-timeout = Looking up the current directory timed out. Shells that report OSC 7 work on any system.
cwd-unavailable = Couldn’t get the current directory. Shells that report OSC 7 work on any system.

## SFTP

sftp-subsystem-failed = Can’t start SFTP on { $endpoint }: { $reason }. The server may have SFTP disabled.
sftp-op-failed = { $op ->
        [list] Can’t list { $path }: { $reason }.
        [stat] Can’t read the attributes of { $path }: { $reason }.
        [mkdir] Can’t create the folder { $path }: { $reason }.
        [remove] Can’t delete { $path }: { $reason }.
        [rename] Can’t rename { $path }: { $reason }.
        [create-file] Can’t create the file { $path }: { $reason }.
        [open] Can’t open { $path }: { $reason }.
       *[other] Can’t complete the operation on { $path }: { $reason }.
    }
sftp-file-exists = { $path } already exists.

## File transfer

xfer-completed = Transferred { $name } ({ $size }).
xfer-cancelled = Transfer cancelled.
xfer-unsafe-name = Refused the file name “{ $name }” because it isn’t safe to write locally.
xfer-size-overflow = The received size doesn’t match the declared size, or exceeds the limit.
xfer-remote-not-dir = { $path } is not a folder.
xfer-local-write-failed = Can’t write { $path }: { $reason }.
xfer-local-read-failed = Can’t read { $path }: { $reason }.
xfer-session-ended = The session ended during the transfer.
xfer-stream-write-failed = Could not write to the terminal connection: { $reason }
xfer-protocol-error = { $protocol ->
        [sftp] SFTP protocol error: { $detail }
        [zmodem] ZMODEM protocol error: { $detail }
        [xmodem] XMODEM protocol error: { $detail }
       *[other] Protocol error: { $detail }
    }

## ZMODEM

zmodem-waiting-sender = Waiting for the sender to start…
zmodem-file-offered = Incoming file: { $name }
zmodem-file-offered-with-size = Incoming file: { $name } ({ $size })
zmodem-file-skipped = Skipped { $name }.
zmodem-receive-summary = { $count ->
        [one] Received { $count } file.
       *[other] Received { $count } files.
    }
zmodem-select-files = Choose the files to send.
zmodem-send-cancelled = Sending cancelled.
zmodem-peer-skipped = The receiver skipped { $name }.
zmodem-resume-unsupported = The receiver asked to resume the transfer, but resuming isn’t supported. Transfer stopped.
zmodem-waiting-confirm = Waiting for the other side to confirm…

## XMODEM

xmodem-timeout = The other side didn’t respond in time.
xmodem-peer-cancelled = The other side cancelled the transfer.

## Import

import-read-failed = Can’t read { $path }: { $reason }.
export-write-failed = Can’t save { $path }: { $reason }.
snippet-limit-reached = You can save up to { $limit } commands. Delete some before adding more.
sender-failed = Sender: { $detail }
sender-empty-command = The command cannot be blank
sender-busy = Another command batch is being sent
sender-no-sessions = No sessions available for sending
sender-repeat-replaced = The previous repeat task has stopped
sender-stopped = Sent { $sent } / { $total } lines
sender-target-closed = Repeat task stopped: the target tab was closed
sender-summary = { $success } successful / { $total } targets{ $skipped ->
    [0] { "" }
   *[other] , { $skipped } skipped
    }
sender-repeat-completed = Repeat task completed ({ $rounds } rounds)
sender-terminal-closed = The terminal was closed
sender-terminal-unavailable = The terminal is disconnected or locked
sender-terminal-transferring = The terminal is transferring files
sender-write-failed = Failed to write to the terminal
import-snippet-converted = Converted “{ $name }”: SFTP-only scope changed to global.
import-wrong-extension = This file has the wrong extension.
import-not-kanso-file = This isn’t a Kanso export file.
import-version-too-new = This file was exported by a newer version (format { $version }). This version supports up to format { $supported }.
import-skipped-unsupported-kind = Skipped “{ $name }”: the type “{ $kind }” isn’t supported.
import-skipped-invalid = Skipped “{ $name }”: { $detail }
import-report = { $added } added, { $updated } updated, { $skipped } skipped.
import-converted-suffix = ; { $count } converted
import-converted-and-skipped = Converted and Skipped
sshconfig-multi-hop-ignored = “{ $alias }” uses a multi-hop ProxyJump. The session was imported without a jump host.
sshconfig-include-too-deep = Include nesting in “{ $file }” is too deep. “{ $include }” was not read.
sshconfig-match-unsupported = “{ $file }” contains a Match block. Match blocks are not evaluated, so their settings do not apply.
sshconfig-invalid-port = “{ $alias }” has an invalid Port “{ $value }”. Port 22 is used instead.
sshconfig-unknown-token = “{ $alias }” uses an unsupported token %{ $token } in { $option }. It was kept as written.
sshconfig-import-report = { $added } added, { $updated } updated, { $jumps_added } jump hosts added, { $multi_hop_ignored } multi-hop ProxyJump entries ignored.

## Credential store

secret-store-failed = Can’t access the credential store: { $detail }
ui-local-shell = Shell
ui-shell-default = System default
ui-shell-custom = Custom command
ui-filter-path = Filter by path
ui-no-matching-paths = No matching paths

ui-session = Session

ui-ssh-connection = SSH Connection

ui-local-session = Local Shell

ui-new-session = New Session

ui-edit-session = Edit Session

ui-type = Type

ui-name = Name

ui-color = Color

ui-group = Group

ui-start-directory = Start Directory

ui-host = Host

ui-port = Port

ui-user = User

ui-authentication = Authentication

ui-password = Password

ui-private-key = Private Key

ui-key-passphrase = Key Passphrase

ui-jump-host = Jump Host

ui-encoding = Encoding

ui-locale = Locale

ui-proxy-command = ProxyCommand

ui-choose = Choose…

ui-cancel = Cancel

ui-save = Save

ui-connect = Connect

ui-automatic = Automatic

ui-keep-saved = Leave empty to keep the saved value

ui-search-sessions = Search saved sessions

ui-saved-sessions = Saved Sessions

ui-no-sessions = No saved sessions yet. Choose “New Session…” to add one.

ui-no-matching-sessions = No matching sessions

ui-empty-group = (Empty)

ui-general = General

ui-display = Display

ui-appearance = Appearance

ui-theme = Mode

ui-light-theme = Light terminal theme

ui-dark-theme = Dark terminal theme

ui-font = Font

ui-terminal-font = Terminal Font

ui-font-size = Font Size

ui-line-height = Line Height

ui-font-custom = Custom font

ui-font-hint = Leave empty to use the system monospaced font. Missing icons use an installed Nerd Font automatically.

ui-font-probing = Detecting installed monospaced fonts…

ui-cursor = Cursor

ui-shape = Shape

ui-cursor-blink = Blinking cursor

ui-semantic-color = Semantic coloring

ui-scrollback = Scrollback

ui-scrollback-lines = Scrollback Lines

ui-scrollback-hint = Lowering this discards the oldest lines immediately.

ui-contrast = Contrast

ui-contrast-enabled = Minimum contrast protection

ui-contrast-ratio = Contrast Ratio

ui-contrast-hint = Text below this contrast ratio is lightened or darkened automatically.

ui-timeline = Timeline

ui-interface-font = Interface Font

ui-settings = Settings
