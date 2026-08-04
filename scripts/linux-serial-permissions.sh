#!/bin/sh
# ser2mcp：Linux 串口权限辅助脚本。
#
# 用途：将指定用户加入 dialout（Debian/Ubuntu）或 uucp（Arch 等）组，
# 使其无需 root 即可访问 /dev/ttyUSB0 等串口设备。
#
# 用法：sudo ./scripts/linux-serial-permissions.sh
#
# 注意：脚本只做标准的用户组配置，不改动任何 udev 规则。

set -e

if [ "$(id -u)" -ne 0 ]; then
  echo "请以 root 运行：sudo $0" >&2
  exit 1
fi

target_user="${SUDO_USER:-root}"
added=0

for group in dialout uucp; do
  if getent group "$group" >/dev/null 2>&1; then
    if usermod -aG "$group" "$target_user"; then
      echo "已将 $target_user 加入 $group 组"
      added=1
    fi
  fi
done

if [ "$added" -eq 0 ]; then
  echo "未找到 dialout/uucp 组，请检查你的发行版文档。" >&2
  exit 1
fi

echo "完成。请注销并重新登录（或重启），然后运行：ser2mcp --list-ports"
