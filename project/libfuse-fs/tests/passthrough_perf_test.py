#!/usr/bin/env python3
import os
import time
import shutil
import random
import string
import statistics
import argparse
from multiprocessing import Pool, cpu_count
from functools import partial

def generate_random_content(size):
    """生成指定大小的随机内容"""
    return ''.join(random.choices(string.ascii_letters + string.digits, k=size)).encode()

def sequential_write_test(test_dir, file_size, file_count):
    """顺序写入测试"""
    content = generate_random_content(file_size)
    start_time = time.time()
    
    for i in range(file_count):
        file_path = os.path.join(test_dir, f"seq_write_{i}.dat")
        with open(file_path, 'wb') as f:
            f.write(content)
    
    elapsed = time.time() - start_time
    total_size = file_size * file_count
    speed = (total_size / 1024 / 1024) / elapsed
    return speed

def sequential_read_test(test_dir, file_count):
    """顺序读取测试"""
    file_list = [os.path.join(test_dir, f"seq_write_{i}.dat") for i in range(file_count)]
    start_time = time.time()
    
    for file_path in file_list:
        with open(file_path, 'rb') as f:
            _ = f.read()
    
    elapsed = time.time() - start_time
    total_size = os.path.getsize(file_list[0]) * file_count
    speed = (total_size / 1024 / 1024) / elapsed
    return speed

def random_write_test(test_dir, file_size, file_count):
    """随机写入测试"""
    content = generate_random_content(file_size)
    start_time = time.time()
    
    for i in range(file_count):
        file_path = os.path.join(test_dir, f"rand_write_{i}.dat")
        with open(file_path, 'wb') as f:
            for _ in range(0, file_size, 4096):  # 4KB块写入
                offset = random.randint(0, file_size-4096)
                f.seek(offset)
                f.write(content[offset:offset+4096])
    
    elapsed = time.time() - start_time
    total_size = file_size * file_count
    speed = (total_size / 1024 / 1024) / elapsed
    return speed

def random_read_test(test_dir, file_count):
    """随机读取测试"""
    file_list = [os.path.join(test_dir, f"rand_write_{i}.dat") for i in range(file_count)]
    file_size = os.path.getsize(file_list[0])
    start_time = time.time()
    
    for file_path in file_list:
        with open(file_path, 'rb') as f:
            for _ in range(100):  # 每个文件随机读取100次
                offset = random.randint(0, file_size-4096)
                f.seek(offset)
                _ = f.read(4096)
    
    elapsed = time.time() - start_time
    total_size = 4096 * 100 * file_count
    speed = (total_size / 1024 / 1024) / elapsed
    return speed

def create_small_files(test_dir, file_size, file_count):
    """创建大量小文件测试"""
    small_dir = os.path.join(test_dir, "small_files")
    os.makedirs(small_dir, exist_ok=True)
    
    content = generate_random_content(file_size)
    start_time = time.time()
    
    for i in range(file_count):
        file_path = os.path.join(small_dir, f"small_{i}.dat")
        with open(file_path, 'wb') as f:
            f.write(content)
    
    elapsed = time.time() - start_time
    ops = file_count / elapsed
    return ops

def stat_small_files(test_dir, file_count):
    """小文件stat测试"""
    small_dir = os.path.join(test_dir, "small_files")
    file_list = [os.path.join(small_dir, f"small_{i}.dat") for i in range(file_count)]
    start_time = time.time()
    
    for file_path in file_list:
        _ = os.stat(file_path)
    
    elapsed = time.time() - start_time
    ops = file_count / elapsed
    return ops

def delete_small_files(test_dir, file_count):
    """小文件删除测试"""
    small_dir = os.path.join(test_dir, "small_files")
    file_list = [os.path.join(small_dir, f"small_{i}.dat") for i in range(file_count)]
    start_time = time.time()
    
    for file_path in file_list:
        os.unlink(file_path)
    
    elapsed = time.time() - start_time
    ops = file_count / elapsed
    return ops

def metadata_operations(test_dir, ops_count):
    """元数据操作测试"""
    meta_dir = os.path.join(test_dir, "metadata_test")
    os.makedirs(meta_dir, exist_ok=True)
    
    start_time = time.time()
    
    for i in range(ops_count):
        dir_path = os.path.join(meta_dir, f"dir_{i}")
        os.mkdir(dir_path)
        os.rename(dir_path, dir_path + "_renamed")
        os.rmdir(dir_path + "_renamed")
    
    elapsed = time.time() - start_time
    ops = ops_count * 3 / elapsed  # 每次循环有3个操作
    return ops

def cleanup(test_dir):
    """清理测试文件"""
    if os.path.exists(test_dir):
        shutil.rmtree(test_dir)

def run_single_test_round(test_dir, file_size, file_count, small_file_size, small_file_count, ops_count, round_num):
    """运行单轮测试"""
    print(f"\n=== 第 {round_num} 轮测试开始 ===")
    os.makedirs(test_dir, exist_ok=True)
    
    results = {}
    
    # 大文件顺序读写测试
    results['seq_write'] = sequential_write_test(test_dir, file_size, file_count)
    print(f"顺序写入速度: {results['seq_write']:.2f} MB/s")
    
    results['seq_read'] = sequential_read_test(test_dir, file_count)
    print(f"顺序读取速度: {results['seq_read']:.2f} MB/s")
    
    # 大文件随机读写测试
    results['rand_write'] = random_write_test(test_dir, file_size, file_count)
    print(f"随机写入速度: {results['rand_write']:.2f} MB/s")
    
    results['rand_read'] = random_read_test(test_dir, file_count)
    print(f"随机读取速度: {results['rand_read']:.2f} MB/s")
    
    # 小文件测试
    results['small_create'] = create_small_files(test_dir, small_file_size, small_file_count)
    print(f"小文件创建速度: {results['small_create']:.2f} 文件/秒")
    
    results['small_stat'] = stat_small_files(test_dir, small_file_count)
    print(f"小文件stat速度: {results['small_stat']:.2f} 操作/秒")
    
    results['small_delete'] = delete_small_files(test_dir, small_file_count)
    print(f"小文件删除速度: {results['small_delete']:.2f} 文件/秒")
    
    # 元数据操作测试
    results['metadata_ops'] = metadata_operations(test_dir, ops_count)
    print(f"元数据操作速度: {results['metadata_ops']:.2f} 操作/秒")
    
    # 清理测试文件
    cleanup(test_dir)
    
    print(f"=== 第 {round_num} 轮测试完成 ===")
    return results

def calculate_statistics(all_results):
    """计算多轮测试的统计信息"""
    stats = {}
    for test_name in all_results[0].keys():
        values = [round_result[test_name] for round_result in all_results]
        stats[test_name] = {
            'avg': statistics.mean(values),
            'min': min(values),
            'max': max(values),
            'stdev': statistics.stdev(values) if len(values) > 1 else 0,
            'values': values
        }
    return stats

def print_final_report(stats, rounds):
    """打印最终测试报告"""
    print("\n=== 最终测试报告 ===")
    print(f"测试轮数: {rounds}")
    print("\n{:<20} {:<10} {:<10} {:<10} {:<10} {:<10}".format(
        "测试项目", "平均值", "最小值", "最大值", "标准差", "单位"))
    
    for test_name, data in stats.items():
        unit = "MB/s" if test_name in ['seq_write', 'seq_read', 'rand_write', 'rand_read'] else "操作/秒"
        print("{:<20} {:<10.2f} {:<10.2f} {:<10.2f} {:<10.2f} {:<10}".format(
            test_name.replace('_', ' ').title(),
            data['avg'],
            data['min'],
            data['max'],
            data['stdev'],
            unit
        ))

def run_tests(test_dir, file_size=10*1024*1024, file_count=10, 
             small_file_size=1024, small_file_count=1000, ops_count=1000, rounds=3):
    """运行多轮测试并计算平均值"""
    print(f"开始文件系统性能测试，测试目录: {test_dir}")
    print(f"测试轮数: {rounds}")
    
    all_results = []
    
    for i in range(rounds):
        round_result = run_single_test_round(
            test_dir, file_size, file_count, 
            small_file_size, small_file_count, ops_count, i+1
        )
        all_results.append(round_result)
    
    stats = calculate_statistics(all_results)
    print_final_report(stats, rounds)
    
    return stats

def main():
    parser = argparse.ArgumentParser(description='文件系统性能测试工具（多轮测试）')
    parser.add_argument('test_dir', help='测试目录路径')
    parser.add_argument('--file-size', type=int, default=10*1024*1024, 
                        help='大文件大小 (bytes), 默认10MB')
    parser.add_argument('--file-count', type=int, default=10, 
                        help='大文件数量, 默认10')
    parser.add_argument('--small-file-size', type=int, default=1024, 
                        help='小文件大小 (bytes), 默认1KB')
    parser.add_argument('--small-file-count', type=int, default=1000, 
                        help='小文件数量, 默认1000')
    parser.add_argument('--ops-count', type=int, default=1000, 
                        help='元数据操作次数, 默认1000')
    parser.add_argument('--rounds', type=int, default=3, 
                        help='测试轮数, 默认3')
    parser.add_argument('--no-cleanup', action='store_true', 
                        help='测试完成后不清理文件')
    
    args = parser.parse_args()
    
    if args.no_cleanup:
        global cleanup
        cleanup = lambda x: None
    
    stats = run_tests(
        args.test_dir,
        file_size=args.file_size,
        file_count=args.file_count,
        small_file_size=args.small_file_size,
        small_file_count=args.small_file_count,
        ops_count=args.ops_count,
        rounds=args.rounds
    )

if __name__ == "__main__":
    main()
