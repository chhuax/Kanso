; Kanso Windows 安装程序（NSIS 3.x，Modern UI 2，Unicode）。
;
; 本文件必须保存为 UTF-8 带 BOM（NSIS 靠 BOM 识别 Unicode 脚本，否则中文串会乱码）。
; 由 `cargo xtask win-package` 调用 makensis 编译，不要手工运行；需要的参数都用 /D 传入：
;   VERSION          产品版本，如 1.0.2
;   VERSION_NUMERIC  四段数字版本，如 1.0.2.0（VIProductVersion 要求）
;   STAGE_DIR        Windows 暂存目录（GPUI Kanso.exe、LICENSE.txt、THIRD_PARTY_NOTICES.windows.md）
;   OUT_FILE         输出的安装程序路径
;   INSTALLER_ICON   安装 / 卸载程序图标（.ico）
;   SIGN_CMD         可选。签名命令模板，%1 会被替换为待签名文件（安装程序与卸载程序都会过一遍），
;                    形如 'signtool sign /fd sha256 /tr <时间戳服务> /td sha256 /a "%1"'。
;                    本版不签名，留空即可；将来接 Authenticode 时只需在 xtask 里设置它。

Unicode true
ManifestDPIAware true
SetCompressor /SOLID lzma
; 用同一份 1.0.2 Windows 载荷对比 8 / 16 / 32 MiB，16 MiB 输出最小。
SetCompressorDictSize 16

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

!ifndef VERSION
  !error "缺少 /DVERSION"
!endif
!ifndef VERSION_NUMERIC
  !error "缺少 /DVERSION_NUMERIC"
!endif
!ifndef STAGE_DIR
  !error "缺少 /DSTAGE_DIR"
!endif
!ifndef OUT_FILE
  !error "缺少 /DOUT_FILE"
!endif
!ifndef INSTALLER_ICON
  !error "缺少 /DINSTALLER_ICON"
!endif

!define APP_NAME "Kanso"
!define APP_EXE "Kanso.exe"
!define PUBLISHER "chhuax"
!define UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Kanso"
!define APP_KEY "Software\Kanso"

Name "${APP_NAME}"
OutFile "${OUT_FILE}"
BrandingText "${APP_NAME} ${VERSION}"

; 按用户安装，不需要管理员权限；默认目录 %LOCALAPPDATA%\Programs\Kanso。
RequestExecutionLevel user
InstallDir "$LOCALAPPDATA\Programs\Kanso"
; 已安装过就沿用上次的目录（原地升级）。
InstallDirRegKey HKCU "${APP_KEY}" "InstallDir"

; 签名钩子：!finalize 在安装程序生成后执行，!uninstfinalize 在卸载程序生成后执行。
!ifdef SIGN_CMD
  !finalize '${SIGN_CMD}'
  !uninstfinalize '${SIGN_CMD}'
!endif

VIProductVersion "${VERSION_NUMERIC}"
VIFileVersion "${VERSION_NUMERIC}"

; ---- 界面 ----
!define MUI_ICON "${INSTALLER_ICON}"
!define MUI_UNICON "${INSTALLER_ICON}"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${STAGE_DIR}\LICENSE.txt"
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE DirectoryLeave
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
; 完成页：第一个复选框启动 Kanso（默认勾选）；第二个（借用“查看自述”位）创建桌面快捷方式（默认不勾选）。
!define MUI_FINISHPAGE_RUN ""
!define MUI_FINISHPAGE_RUN_FUNCTION LaunchKanso
!define MUI_FINISHPAGE_SHOWREADME ""
!define MUI_FINISHPAGE_SHOWREADME_TEXT "$(STR_DESKTOP_SHORTCUT)"
!define MUI_FINISHPAGE_SHOWREADME_FUNCTION CreateDesktopShortcut
!define MUI_FINISHPAGE_SHOWREADME_NOTCHECKED
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

; 语言：未命中系统界面语言时回退到第一个（English）；系统是简体中文时自动选简体中文，不弹语言选择框。
!insertmacro MUI_LANGUAGE "English"
!insertmacro MUI_LANGUAGE "SimpChinese"

VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "LegalCopyright" "Copyright (C) ${PUBLISHER}. GPL-3.0-or-later."
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileDescription" "${APP_NAME} Setup"
VIAddVersionKey /LANG=${LANG_ENGLISH} "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_ENGLISH} "ProductVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_SIMPCHINESE} "ProductName" "${APP_NAME}"
VIAddVersionKey /LANG=${LANG_SIMPCHINESE} "CompanyName" "${PUBLISHER}"
VIAddVersionKey /LANG=${LANG_SIMPCHINESE} "LegalCopyright" "Copyright (C) ${PUBLISHER}. GPL-3.0-or-later."
VIAddVersionKey /LANG=${LANG_SIMPCHINESE} "FileDescription" "${APP_NAME} 安装程序"
VIAddVersionKey /LANG=${LANG_SIMPCHINESE} "FileVersion" "${VERSION}"
VIAddVersionKey /LANG=${LANG_SIMPCHINESE} "ProductVersion" "${VERSION}"

LangString STR_DESKTOP_SHORTCUT ${LANG_ENGLISH} "Create a desktop shortcut"
LangString STR_DESKTOP_SHORTCUT ${LANG_SIMPCHINESE} "创建桌面快捷方式"
LangString STR_RUNNING ${LANG_ENGLISH} "${APP_NAME} is running. Please close it, then click Retry. Click Cancel to stop."
LangString STR_RUNNING ${LANG_SIMPCHINESE} "${APP_NAME} 正在运行。请先关闭它，再点“重试”；点“取消”则中止。"
LangString STR_PURGE ${LANG_ENGLISH} "Also delete your Kanso data (saved sessions, settings and logs in $LOCALAPPDATA\Kanso)?$\r$\n$\r$\nPasswords stored in Windows Credential Manager are not removed.$\r$\n$\r$\nChoose No to keep your data."
LangString STR_PURGE ${LANG_SIMPCHINESE} "是否同时删除 Kanso 的用户数据（$LOCALAPPDATA\Kanso 中的会话、设置与日志）？$\r$\n$\r$\n保存在 Windows 凭据管理器中的密码不会被删除。$\r$\n$\r$\n选“否”则保留数据。"

; ---- 检测 Kanso 是否正在运行 ----
; 做法：以写入方式打开 Kanso.exe。正在运行的 exe 被系统锁定，打开会失败；失败就提示用户关闭，绝不强杀进程。
!macro CheckRunningMacro PREFIX
  Function ${PREFIX}CheckRunning
    IfFileExists "$INSTDIR\${APP_EXE}" 0 done
    retry:
    ClearErrors
    FileOpen $0 "$INSTDIR\${APP_EXE}" a
    ${IfNot} ${Errors}
      FileClose $0
      Goto done
    ${EndIf}
    ; /SD IDCANCEL：静默模式下无人可以关程序，直接中止。
    MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "$(STR_RUNNING)" /SD IDCANCEL IDRETRY retry
    Abort
    done:
  FunctionEnd
!macroend
!insertmacro CheckRunningMacro ""
!insertmacro CheckRunningMacro "un."

; 选目录页离开时检查：Abort 会留在这一页，用户关掉 Kanso 后可以再点“下一步”。
Function DirectoryLeave
  Call CheckRunning
FunctionEnd

Function LaunchKanso
  Exec '"$INSTDIR\${APP_EXE}"'
FunctionEnd

Function CreateDesktopShortcut
  CreateShortcut "$DESKTOP\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0
FunctionEnd

; ---- 安装 ----
Section "Kanso" SecMain
  SectionIn RO
  ; 静默安装（/S）没有选目录页，这里再检查一次；失败则整个安装中止。
  Call CheckRunning

  SetOutPath "$INSTDIR"
  File "${STAGE_DIR}\${APP_EXE}"
  File "${STAGE_DIR}\LICENSE.txt"
  File "${STAGE_DIR}\THIRD_PARTY_NOTICES.windows.md"
  ; 旧 Windows 安装曾附带 macOS 声明。升级时只清理这一个已知的旧发行文件。
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateShortcut "$SMPROGRAMS\${APP_NAME}.lnk" "$INSTDIR\${APP_EXE}" "" "$INSTDIR\${APP_EXE}" 0

  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayName" "${APP_NAME}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${UNINST_KEY}" "Publisher" "${PUBLISHER}"
  WriteRegStr HKCU "${UNINST_KEY}" "DisplayIcon" "$INSTDIR\${APP_EXE},0"
  WriteRegStr HKCU "${UNINST_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
  WriteRegStr HKCU "${UNINST_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
  WriteRegStr HKCU "${UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${UNINST_KEY}" "URLInfoAbout" "https://github.com/chhuax/Kanso"
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${UNINST_KEY}" "NoRepair" 1
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  IntFmt $0 "0x%08X" $0
  WriteRegDWORD HKCU "${UNINST_KEY}" "EstimatedSize" "$0"
SectionEnd

; ---- 卸载 ----
Function un.onInit
  Call un.CheckRunning
FunctionEnd

Section "Uninstall"
  Delete "$INSTDIR\${APP_EXE}"
  Delete "$INSTDIR\LICENSE.txt"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.md"
  Delete "$INSTDIR\THIRD_PARTY_NOTICES.windows.md"
  Delete "$INSTDIR\Uninstall.exe"
  Delete "$SMPROGRAMS\${APP_NAME}.lnk"
  Delete "$DESKTOP\${APP_NAME}.lnk"
  ; 只删空目录，不递归：用户若往安装目录里放了别的文件，保持原样。
  RMDir "$INSTDIR"

  DeleteRegKey HKCU "${UNINST_KEY}"
  DeleteRegKey HKCU "${APP_KEY}"

  ; 默认保留用户数据；静默卸载（/S）一律保留。
  MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "$(STR_PURGE)" /SD IDNO IDNO keep_data
  RMDir /r "$LOCALAPPDATA\Kanso"
  keep_data:
SectionEnd
