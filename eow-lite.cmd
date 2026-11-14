@echo off
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0eow-lite.ps1" %*
exit /b %errorlevel%
