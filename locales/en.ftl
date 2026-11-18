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
ui-open-themes-folder = Open Themes Folder

ui-font = Font

ui-terminal-font = Terminal Font

ui-font-size = Font Size

ui-line-height = Line Height

ui-font-custom = Custom font

ui-font-hint = Leave empty to use the system monospaced font. Missing icons use an installed Nerd Font automatically.

ui-missing-terminal-font = “{ $name }” isn’t installed. The system monospaced font is used instead.
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
ui-go-to-tab = Go to Tab
ui-tab-number = Tab { $number }
ui-gutter-menu = Timestamps and Line Numbers

ui-interface-font = Interface Font

ui-settings = Settings

ui-sender-title = Sender
ui-sender-run = Run
ui-sender-stop = Stop
ui-sender-clear = Clear
ui-sender-repeat = Repeat
ui-sender-save = Save
ui-sender-update = Update
ui-sender-cancel-edit = Cancel Edit
ui-sender-name-placeholder = Name (optional)
ui-sender-input-placeholder = Type a command. Enter sends, Shift+Enter adds a line
ui-sender-interval = Interval (s)
ui-sender-count = Count (0 = unlimited)
ui-sender-library-empty = No saved commands
ui-sender-library-none = No commands apply to this session
ui-sender-prev-page = Previous
ui-sender-next-page = Next
ui-sender-ending-none = No line ending
ui-sender-target-current = Current session ({ $count })
ui-sender-target-all = All sessions ({ $count })
ui-sender-target-all-label = All sessions
ui-sender-target-current-label = Current session · { $name }
ui-sender-scope-global = Global
ui-sender-scope-ssh = All SSH sessions
ui-sender-scope-local = All local shell sessions
ui-sender-scope-group = Group · { $name }
ui-sender-scope-profile = Session · { $name }
ui-sender-menu-edit = Edit
ui-sender-menu-repeat = Repeat
ui-sender-menu-ending = Line Ending
ui-sender-menu-scope = Scope
ui-sender-menu-delete = Delete
ui-sender-rounds = { $rounds } rounds sent
ui-sender-rounds-of = { $rounds } / { $count } rounds sent
ui-sender-sending = { $progress } · sending
ui-sender-next-in = { $progress } · next in { $wait } s
ui-sender-repeat-bar = Repeating: { $title } · every { $interval } s · { $dest } · { $progress }
ui-sender-chip = Repeating · { $rounds } rounds
ui-sender-chip-of = Repeating · { $rounds }/{ $count }
ui-sender-join = ; 
ui-sender-save-failed = Failed to save command: { $detail }
ui-sender-update-failed = Failed to update command: { $detail }
ui-sender-delete-failed = Failed to delete command: { $detail }
ui-sender-send-failed = Failed to send: { $detail }
ui-sender-limit = The library is full ({ $max }); you can still update or delete
ui-completion-directory = Directory
ui-completion-file = File
ui-completion-history = History
ui-completion-kubernetes = Kubernetes
ui-completion-docker = Docker
ui-completion-git = Git
ui-completion-systemd = systemd
ui-menu-command-suggestions = Command Suggestions
ui-menu-clear-history = Clear Command History…
ui-menu-sender = Sender
ui-history-clear-title = Clear Command History?
ui-history-clear-body = Clear recorded command history from all terminals. This can’t be undone.
ui-history-clear-confirm = Clear
ui-history-clear-cancel = Cancel
ui-history-clear-failed = Failed to clear command history: { $detail }
ui-follow-system = Follow system

ui-language = Language

ui-interface-language = Interface language

ui-takes-effect-immediately-follow-system = Takes effect immediately. “Follow system” uses the system display language; unsupported languages fall back to English.

ui-export-complete = Export Complete

ui-exported-to-file-contains-no = Exported to { $path }.
    The file contains no passwords or passphrases.

ui-import-data = Import Data

ui-this-file-will-be-imported = This file will be imported:

ui-data-is-merged-by-id = Data is merged by id: sessions, groups and commands with the same id replace the local ones, everything else is added, and no local data is deleted. Replaced sessions keep their saved credentials. Invalid entries are skipped.

ui-import-complete = Import Complete

ui-operation-failed = Operation Failed

ui-skipped = Skipped

ui-warnings = Warnings

ui-ok = OK

ui-import = Import

ui-choose-kanso-file-to-import = Choose a .kanso file to import

ui-import-openssh-config = Import OpenSSH Config

ui-import-openssh-config-2 = Import OpenSSH Config…

ui-choose-openssh-config-file = Choose an OpenSSH config file

ui-no-group = No group

ui-import-into-group = Import into group

ui-select-all-of-selected = Select all ({ $selected } of { $total } selected)

ui-saved-will-update = Saved, will update

ui-key = Key { $path }

ui-via = via { $jump }

ui-reading = Reading…

ui-importing = Importing…

ui-no-hosts-to-import-in = No hosts to import in this file.

ui-found-hosts-of-them-already = Found { $found } hosts, { $existing } of them already saved (they will be updated).

ui-no-open-sessions = No open sessions

ui-new-local-shell = New Local Shell

ui-new-session-2 = New Session…

ui-or-press = or press

ui-previous-tab = Previous Tab

ui-next-tab = Next Tab

ui-previous-pane = Previous Pane

ui-next-pane = Next Pane

ui-close-session = Close Session

ui-export-data = Export Data…

ui-import-data-2 = Import Data…

ui-copy = Copy

ui-paste = Paste

ui-select-all = Select All

ui-clear-buffer = Clear Buffer

ui-split-right = Split Right

ui-split-down = Split Down

ui-right-click-shows-menu = Right-click Shows Menu

ui-tab-bar = Tab Bar

ui-view = View

ui-settings-2 = Settings…

ui-quit = Quit

ui-edit-session-2 = Edit Session…

ui-close = Close

ui-close-others = Close Others

ui-close-tabs-to-left = Close Tabs to the Left

ui-close-tabs-to-right = Close Tabs to the Right

ui-close-all = Close All

ui-and-more = …and { $count } more

ui-quit-kanso = Quit Kanso?

ui-close-running-sessions = Close running sessions?

ui-these-sessions-are-still-running = These sessions are still running or connected:

ui-closing-will-interrupt-file-transfers = Closing will interrupt file transfers in progress.

ui-connecting = Connecting…

ui-session-ended-exit-code = Session ended (exit code { $code })

ui-session-ended = Session ended

ui-session-disconnected = Session disconnected

ui-command-running = Command running

ui-ai-running = AI running

ui-ai-finished = AI finished

ui-command-finished-click-to-view = Command finished, click to view

ui-operation-failed-2 = The operation failed.

ui-default-shell = Default shell

ui-edit = Edit…

ui-move-to-group = Move to Group

ui-delete = Delete…

ui-rename = Rename…

ui-new-group = New Group…

ui-deleting-this-group-also-deletes = Deleting this group also deletes its { $profiles } sessions and { $snippets } snippets. This can’t be undone.

ui-only-session-and-its-credentials = Only the session and its credentials saved in Kanso are deleted; ~/.ssh/config and key files are untouched. This can’t be undone.

ui-local = Local

ui-enter-session-name = Enter a session name.

ui-port-must-be-number-from = The port must be a number from 1 to 65535.

ui-locale-may-only-contain-letters = Locale may only contain letters, digits and _ - . @, 1 to 64 characters.

ui-save-failed = Save failed.

ui-session-saved-but-credentials-could = Session saved, but the credentials could not be stored: { $detail }

ui-server = Server: { $host_port }

ui-server-via = Server: { $host_port } (via { $via })

ui-tab = Tab: { $title }

ui-host-key-changed = Host Key Changed

ui-key-type = Key type

ui-sha256-fingerprint = SHA256 fingerprint

ui-previously-recorded-at = Previously recorded at

ui-accept-new-key-and-connect = Accept New Key and Connect

ui-verification-required = Verification Required

ui-continue = Continue

ui-private-key-passphrase-required = Private Key Passphrase Required

ui-credential-intro-key = { $server }
    User: { $user }
    Key: { $key }

ui-password-required = Password Required

ui-credential-intro = { $server }
    User: { $user }

ui-connect-and-save = Connect and Save

ui-connect-only = Connect Only

ui-block = Block

ui-underline = Underline

ui-bar = Bar

ui-light = Light

ui-dark = Dark

ui-line-numbers-only = Line numbers only

ui-timestamps-only = Timestamps only

ui-timestamps-and-line-numbers = Timestamps and line numbers

ui-off = Off

ui-cant-open-local-shell = Can’t open a local shell: { $detail }

ui-cant-split = Can’t split: { $detail }

ui-session-ended-press-ctrl-shift = Session ended. Press Ctrl+Shift+W to close this tab

ui-session-ended-exit-code-press = Session ended (exit code { $code }). Press Ctrl+Shift+W to close this tab

ui-session-disconnected-press-ctrl-shift = Session disconnected. Press Ctrl+Shift+W to close this tab

ui-this-session-no-longer-exists = This session no longer exists

ui-sftp-sessions-need-file-panel = SFTP sessions need the file panel, which the main window can’t open yet

ui-cant-connect-to = Can’t connect to “{ $name }”: { $detail }

ui-pane = Pane { $index }

ui-files = Files

ui-timestamps = Timestamps

ui-find-placeholder = Find

ui-find-no-results = No results

ui-find-invalid-regex = Invalid regular expression

ui-copy-command = Copy command

ui-copy-output = Copy output

ui-copied = Copied

ui-link-unsupported = This kind of link can't be opened

ui-new-group-2 = New Group

ui-group-name = Group name

ui-create = Create

ui-rename-group = Rename Group

ui-rename-2 = Rename

ui-delete-2 = Delete

ui-delete-group = Delete group “{ $name }”?

ui-delete-session = Delete session “{ $name }”?

ui-edit-2 = Edit

ui-none = None

ui-choose-private-key-file = Choose a private key file

ui-search-fonts = Search fonts

ui-search-monospaced-fonts = Search monospaced fonts

ui-system-default-or-type-font = System default or font name

ui-lines = { $lines } lines
## Filer 与文件传输（GPUI，自动生成区块，改动请编辑生成脚本的清单）
ui-xfer-default-name = received.bin
ui-xfer-hint-pick-files = The remote side is ready to receive. Choose the files to send.
ui-xfer-hint-pick-file = Choose the file to send with XMODEM.
ui-xfer-hint-save-sized = The remote side is sending { $name } ({ $amount }). Choose where to save it.
ui-xfer-hint-save = The remote side is sending { $name }. Choose where to save it.
ui-xfer-hint-xmodem-save = Choose where to save the file received with XMODEM.
ui-xfer-done = Transfer complete
ui-xfer-cancelled = Cancelled
ui-xfer-failed = Failed: { $why }
ui-xfer-send = send
ui-xfer-receive = receive
ui-filer-delete-link = Delete Link
ui-filer-delete-link-note = Only the link is removed, not its target. This cannot be undone.
ui-filer-delete-dir = Delete Folder
ui-filer-delete-dir-note = Only empty folders can be deleted. This cannot be undone.
ui-filer-delete-file = Delete File
ui-filer-delete-file-note = The file will be removed. This cannot be undone.
ui-filer-delete-where = Location: { $location }
ui-filer-delete-many = Delete { $count } items
ui-filer-delete-more = { $count } more
ui-filer-delete-many-note = These items will be permanently removed. This cannot be undone; folders that are not empty are kept.
ui-filer-cancel = Cancel
ui-filer-delete = Delete
ui-filer-new-file = New File
ui-filer-new-folder = New Folder
ui-filer-rename = Rename
ui-filer-name-where = Location: { $location }
ui-filer-name = Name
ui-filer-ok = OK
ui-filer-name-empty = The name can't be empty.
ui-filer-name-reserved = The name can't be . or ..
ui-filer-name-separator = The name can't contain a path separator; it must be a single item in the current folder.
ui-filer-name-invalid-char = The name can't contain the character { $ch }
ui-filer-conflict-title = Item Already Exists
ui-filer-conflict-file = A file named “{ $name }” already exists at the destination.
ui-filer-conflict-dir = A folder named “{ $name }” already exists at the destination.
ui-filer-conflict-merge-note = Folders are merged; files with the same name inside are handled by your choice.
ui-filer-skip = Skip
ui-filer-skip-all = Skip All
ui-filer-overwrite = Replace
ui-filer-overwrite-all = Replace All
ui-filer-err-not-found = Not found
ui-filer-err-denied = Permission denied
ui-filer-err-not-dir = Not a folder
ui-filer-err-exists = Already exists
ui-filer-err-not-empty = The folder isn't empty; only empty folders can be deleted
ui-filer-err-cancelled = Cancelled
ui-filer-drive = Local Disk ({ $letter }:)
ui-filer-err-copy-into-self = A folder can't be copied into itself or one of its subfolders
ui-filer-err-no-name = Can't get the name
ui-filer-err-target-link = The destination is a symbolic link; writing was refused
ui-filer-err-target-dir = A folder with the same name exists at the destination
ui-filer-this-pc = This PC
ui-filer-connecting = Connecting…
ui-filer-loading = Loading… { $count } received
ui-filer-disconnected-stats = Disconnected (the list is out of date, { $count } items)
ui-filer-empty = { $count } items
ui-filer-stats = { $items } items: { $dirs } folders, { $files } files
ui-filer-stats-links = { $items } items: { $dirs } folders, { $files } files, { $links } links
ui-filer-kind-link = Link
ui-filer-kind-dir = Folder
ui-filer-kind-file = File
ui-filer-tip-kind = Type: { $kind }
ui-filer-tip-size = Size: { $amount }
ui-filer-tip-perm = Permissions: { $perm }
ui-filer-tip-owner = Owner: { $owner }
ui-filer-tip-group = Group: { $group }
ui-filer-op-upload = Upload
ui-filer-op-download = Download
ui-filer-op-copy = Copy
ui-filer-busy = Busy. Wait for the current transfer to finish.
ui-filer-batch-done = Done: { $count } items
ui-filer-cancelled = Cancelled
ui-filer-op-failed = { $action } failed: { $why }
ui-filer-op-error = { $action } { $name }: { $why }
ui-filer-awaiting-conflict = Waiting for a decision on a duplicate name…
ui-filer-skipped = { $count } duplicates skipped
ui-filer-local = Local
ui-filer-identity = { $name } · { $host }
ui-filer-not-remote = This pane isn't remote, so it can't upload or download.
ui-filer-no-write-this-pc = “This PC” isn't a real folder; you can't create, delete or receive files here.
ui-filer-disconnected-write = The connection is closed; write operations are stopped.
ui-filer-no-dir-open = No folder is open, or reading it failed.
ui-filer-already-here = The item is already in this folder; skipped.
ui-filer-link-broken = Can't open the link “{ $name }”: { $why }
ui-filer-remote-edit-unsupported = Editing remote files with a local app isn't supported yet; use Download in the context menu.
ui-filer-menu-open = Open
ui-filer-menu-open-with = Open With…
ui-filer-menu-download = Download
ui-filer-menu-copy-path = Copy Full Path
ui-filer-menu-copy-name = Copy Name
ui-filer-menu-copy-dir = Copy Current Folder Path
ui-filer-upload-files = Upload Files…
ui-filer-upload-dirs = Upload Folders…
ui-filer-refresh = Refresh
ui-filer-op-error-at = { $what } “{ $name }” failed ({ $place }): { $why }
ui-filer-tip-upload = Upload
ui-filer-tip-upload-remote-only = Only available for connected SSH sessions
ui-filer-tip-hidden = Show / hide items starting with a dot
ui-filer-tip-locate = Show the terminal's folder
ui-filer-tip-up = Up
ui-filer-tip-home = Home
ui-filer-tip-cancel = Cancel transfer
ui-filer-col-name = Name
ui-filer-col-date = Modified
ui-filer-col-size = Size
ui-filer-col-time = Modified
ui-filer-folder-empty = This folder is empty
ui-filer-open-failed = Can't open { $dest }: { $why }
ui-filer-still-showing = The list below is still { $path }
ui-filer-go-home = Go to Home Folder
ui-filer-disconnected-badge = Disconnected
ui-filer-drop-upload = Drop to upload
ui-filer-drop-copy = Drop to copy here
ui-filer-dismiss = Dismiss
ui-filer-send-to-remote = Upload to remote
ui-filer-get-from-remote = Download to local
ui-filer-no-session = No session to browse
ui-filer-menu-locate = Show Working Folder in Filer
ui-filer-menu-view = File Browser
ui-filer-status-label = Files
ui-filer-status-already-open = File browsing is already open in this tab.
ui-filer-located = Showing { $path }
ui-filer-locate-no-session = No session to locate.
ui-filer-locate-sftp = A file session has no shell working folder.
ui-filer-locate-not-connected = The session isn't connected.
ui-filer-locate-foreign-host = The folder reported by the terminal belongs to another host ({ $host}); it can't be used as a local path.
ui-filer-locate-failed = Can't get the working folder of “{ $name }”: { $why }
ui-xfer-menu = File Transfer
ui-xfer-menu-send = Send with XMODEM…
ui-xfer-menu-send-1k = Send with XMODEM-1K…
ui-xfer-menu-receive = Receive with XMODEM…
ui-xfer-menu-cancel = Cancel Transfer
ui-xfer-start-failed = Can't start the transfer: { $why }

ui-search = Search

ui-file-browser = File Browser

ui-palette-tabs = Tabs
ui-palette-sessions = Sessions
ui-palette-commands = Commands
ui-palette-hint = Search tabs, sessions, commands, files
ui-search-empty = No matches

ui-git-title = Changes
ui-git-refresh = Refresh Changes
ui-git-close = Close Changes Panel
ui-git-more = More Actions
ui-git-unsupported = Can’t detect changes in this directory; Git repositories only
ui-git-loading = Reading Changes…
ui-git-clean = Working Tree Clean
ui-git-commit-placeholder = Commit Message (Return to Commit)
ui-git-commit = Commit
ui-git-discard-file = Discard Changes to This File
ui-git-discard-all = Discard All Changes
ui-git-copy-path = Copy Full Path
ui-git-reading-diff = Reading Diff…
ui-git-diff-truncated = Diff Truncated
ui-git-commit-may-staged = The commit may have left changes staged.
ui-git-discard-title-file = Discard changes to “{ $path }”?
ui-git-discard-title-all = Discard all changes in “{ $repo }”?
ui-git-discard-body-new = New content will be permanently deleted.
ui-git-discard-body-tracked = Staged and unstaged edits will be lost; renames will restore the original path.
ui-git-discard-body-all = Staged, unstaged, and untracked changes will be permanently lost. Git-ignored files are kept.
ui-git-discard-no-undo = There is no in-app undo, Trash, or automatic backup.
ui-git-cancel = Cancel
ui-git-discard = Discard
ui-git-menu-view = Git Changes
ui-git-branch-tooltip = { $repo } · { $branch }

## 键盘设置页与恢复默认设置（GPUI）
ui-keys-page = Keyboard
ui-keys-group-other = Other
ui-keys-hint = Click a shortcut, then press the new keys. Esc cancels, Backspace clears.
ui-keys-recording = Press keys…
ui-keys-unbound = Not set
ui-keys-fixed-tabs = Go to Tab 1–9
ui-keys-fixed-tabs-key = Alt+1–Alt+9
ui-keys-reserved = { $chord } is reserved by the system or fixed, and can’t be assigned.
ui-keys-needs-modifier = A shortcut needs Ctrl or Alt. Function keys can be used alone; arrow keys and similar need at least one modifier.
ui-keys-conflict-title = { $chord } is already in use
ui-keys-conflict-body = It is assigned to “{ $holder }”. Replace it to use { $chord } for “{ $new }” instead.
ui-keys-replace = Replace
ui-keys-restore-item = Restore default
ui-keys-restore-all = Restore All Defaults
ui-keys-customized = Customized
ui-help-menu = Help
ui-about-kanso = About Kanso
ui-about-version = Version { $version }
ui-about-license = Licensed under GPL-3.0-or-later
ui-open-source-code = View Source
ui-open-licenses = Open Source Licenses…
ui-reset-menu = Restore Default Settings…
ui-reset-title = Restore default settings?
ui-reset-body = Display, appearance and keyboard shortcut settings will be reset. Sessions, groups, snippets and history are not affected.
ui-reset-confirm = Restore
ui-reset-failed = Couldn’t restore default settings: { $detail }
ui-reset-done = Default settings restored
ui-keys-cmd-open-launcher = Session Launcher
ui-keys-cmd-find = Find…
ui-keys-cmd-find-next = Find Next
ui-keys-cmd-find-prev = Find Previous
ui-keys-cmd-prev-block = Previous Command Block
ui-keys-cmd-next-block = Next Command Block
ui-keys-cmd-toggle-sidebar = Toggle Tab Bar
ui-keys-cmd-toggle-sender = Toggle Sender
ui-keys-cmd-toggle-filer = Toggle File Browser
ui-keys-cmd-zoom-in = Bigger Text
ui-keys-cmd-zoom-out = Smaller Text
ui-keys-cmd-zoom-reset = Default Text Size

ui-keys-cmd-open-palette = Command Search

ui-redit-preparing = Preparing remote edit: { $name }
ui-redit-synced = Synced
ui-redit-pending = Waiting to sync
ui-redit-syncing = Syncing…
ui-redit-conflict = Remote file changed; sync paused
ui-redit-failed = Sync failed: { $why }
ui-redit-status-line = { $source } · { $path } · { $text }
ui-redit-open-failed = Can't open “{ $name }”: { $why }
ui-redit-not-connected = The session isn't connected, so the remote file can't be opened.
ui-redit-bad-name = “{ $name }” contains characters this computer doesn't allow in file names, so it can't be edited locally; download it instead.
ui-redit-unsaved-kept = “{ $name }” has local changes that haven't been uploaded, so it wasn't downloaded again; the local copy was reopened.
ui-redit-retained = Changes did not reach the server: { $count } local copies were kept in { $dir }
ui-redit-large-title = Open a large file?
ui-redit-large-body = “{ $name }” is { $amount } and must be fully downloaded before it can be edited.
ui-redit-large-continue = Download and Open
ui-redit-conflict-title = The remote file has changed
ui-redit-conflict-body = “{ $name }” ({ $source }) was modified or deleted on the server after you opened it. Uploading now would overwrite what is there.
ui-redit-conflict-overwrite = Overwrite Remote
ui-redit-conflict-redownload = Discard Mine and Re-download
ui-redit-conflict-pause = Decide Later
ui-filer-drag-preparing = Preparing { $name } for drag
ui-filer-drag-failed = Can't prepare “{ $name }” for drag: { $why }
ui-filer-drag-too-late = Preparation finished after the drag ended; drag again.
ui-filer-drag-not-ready = The remote file wasn't ready and was cancelled; keep the button held until it is ready.
ui-filer-drag-cancelled = Drag preparation cancelled

ui-display-settings-menu = Display Settings…
ui-keyboard-settings-menu = Keyboard Shortcuts…
