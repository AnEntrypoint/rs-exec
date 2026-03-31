$ErrorActionPreference = "Stop"
$rustup = "$env:USERPROFILE\.cargo\bin\rustup.exe"
if (!(Test-Path $rustup)) { throw "rustup.exe not found at $rustup" }
& $rustup run stable-x86_64-pc-windows-msvc cargo build --release @args
