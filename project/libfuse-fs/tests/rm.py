import os
import sys
import argparse

#!/usr/bin/env python3

def remove_file(path, recursive=False, force=False):
    """Remove a file or directory."""
    try:
        if os.path.isdir(path):
            if not recursive:
                print(f"rm: cannot remove '{path}': Is a directory", file=sys.stderr)
                return False
            for item in os.listdir(path):
                item_path = os.path.join(path, item)
                remove_file(item_path, recursive=True, force=force)
            os.rmdir(path)
        else:
            os.remove(path)
        return True
    except FileNotFoundError:
        if not force:
            print(f"rm: cannot remove '{path}': No such file or directory", file=sys.stderr)
        return False
    except PermissionError:
        if not force:
            print(f"rm: cannot remove '{path}': Permission denied", file=sys.stderr)
        return False

def main():
    parser = argparse.ArgumentParser(description='Remove files or directories')
    parser.add_argument('-r', '-R', '--recursive', action='store_true',
                       help='remove directories and their contents recursively')
    parser.add_argument('-f', '--force', action='store_true',
                       help='ignore nonexistent files and arguments, never prompt')
    parser.add_argument('paths', nargs='+', help='paths to remove')

    args = parser.parse_args()

    exit_code = 0
    for path in args.paths:
        if not remove_file(path, args.recursive, args.force):
            exit_code = 1

    sys.exit(exit_code)

if __name__ == '__main__':
    main()