#!/bin/bash

# Buck2 清理脚本：停止 buck2d 并卸载指定挂载点

# 错误处理
set -euo pipefail

# 定义函数：停止 buck2d 进程
stop_buck2d() {
    echo "正在查找并停止 buck2d 进程..."
    
    # 查找 buck2d 进程ID
    local pids=$(pgrep -f "buck2d")
    
    if [ -z "$pids" ]; then
        echo "未找到 buck2d 进程"
        return 0
    fi
    
    echo "找到以下 buck2d 进程：$pids"
    
    # 尝试优雅终止
    echo "发送 SIGTERM 信号..."
    kill $pids || true
    
    # 等待最多10秒
    local timeout=10
    while [ $timeout -gt 0 ]; do
        if ! pgrep -f "buck2d" >/dev/null; then
            echo "buck2d 已成功停止"
            return 0
        fi
        echo "等待 buck2d 进程退出... ($timeout秒)"
        sleep 1
        timeout=$((timeout-1))
    done
    
    # 强制终止
    echo "发送 SIGKILL 信号..."
    kill -9 $pids || true
    
    # 再次检查
    if ! pgrep -f "buck2d" >/dev/null; then
        echo "buck2d 已强制停止"
        return 0
    else
        echo "警告：无法停止 buck2d 进程"
        return 1
    fi
}

# 定义函数：卸载挂载点
unmount_path() {
    local mount_point="$1"
    
    if [ -z "$mount_point" ]; then
        echo "错误：未指定挂载点"
        return 1
    fi
    
    echo "检查挂载点：$mount_point"
    
    # 检查是否为有效挂载点
    
    echo "正在卸载挂载点：$mount_point"
    
    # 尝试普通卸载
    if umount "$mount_point"; then
        echo "卸载成功"
        return 0
    fi
    
    echo "普通卸载失败，尝试强制卸载..."
    
    # 尝试强制卸载
    if umount -l "$mount_point"; then
        echo "延迟卸载成功"
        return 0
    fi
    
    echo "错误：无法卸载 $mount_point"
    return 1
}

# 主函数
main() {
    local mount_point="/home/luxian/megatest/true_temp"
    
    # 解析参数
    while [[ $# -gt 0 ]]; do
        case $1 in
            -m|--mount)
                mount_point="$2"
                shift 2
                ;;
            *)
                echo "未知参数：$1"
                echo "用法：$0 --mount /path/to/mount-point"
                return 1
                ;;
        esac
    done
    
    # 检查是否提供了挂载点
    if [ -z "$mount_point" ]; then
        echo "错误：必须指定挂载点"
        echo "用法：$0 --mount /path/to/mount-point"
        return 1
    fi
    
    # 检查是否为root用户（或使用sudo）
    if [ "$(id -u)" -ne 0 ]; then
        echo "警告：建议使用 sudo 运行此脚本以确保权限足够"
    fi
    
    # 执行清理
    stop_buck2d
    unmount_path "$mount_point"
    
    echo "清理完成"
}

# 执行主函数
main "$@"    