@echo off
setlocal DisableDelayedExpansion
echo {"type":"system","subtype":"init","session_id":"40000000-0000-4000-8000-000000000000","model":"fake-claude","permissionMode":"default"}
:read
set "line="
set /p "line="
if errorlevel 1 exit /b 0
>>"%NMT_FAKE_STREAM_LOG%" echo %line%
goto read
