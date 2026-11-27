#!/bin/sh
set -eu
cd "$(dirname "$0")"
out=$1
llvm-dlltool -m i386:x86-64 -d kernel32.def -l "$out/kernel32.lib"
clang --target=x86_64-windows-msvc -Oz -ffreestanding -fno-stack-protector -c supervisor.c -o "$out/supervisor.obj"
lld-link /timestamp:0 /subsystem:windows /entry:entry /nodefaultlib "/out:$out/supervisor.exe" "$out/supervisor.obj" "$out/kernel32.lib"
