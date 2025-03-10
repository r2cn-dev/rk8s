#!/bin/bash
set -e

cd "$(dirname "$0")/../../../"

echo "Setting up Git configuration..."
git config --local user.name "GitHub Actions"
git config --local user.email "actions@github.com"

echo "Fetching the latest youki repository..."
if ! git remote | grep -q "youki"; then
    git remote add youki https://github.com/youki-dev/youki.git
fi
git fetch youki || { echo "Failed to fetch youki repository."; exit 1; }

check_diff() {
    local module=$1
 
    mkdir -p "$module"
    echo "Checking differences for $module..."
    
    echo "Git diff for $module:"
    git diff --stat youki/main -- "crates/$module" "project/$module" 
  
}

check_diff "libcontainer"
check_diff "libcgroups"

echo -n "Do you want to proceed with the sync? (y/n): "
read CONFIRMATION

if [[ "$CONFIRMATION" != "y" ]]; then
    echo "Sync canceled."
    exit 0
fi

sync_module() {
    local module=$1
    local source_path="crates/$module"
    local target_path="project/$module"

    echo "Syncing $module from youki..."

    temp_dir=$(mktemp -d)

    git worktree add "$temp_dir" youki/main || {
        echo "Failed to checkout youki/main into temporary worktree"
        return 1
    }

    rsync -a --delete "$temp_dir/$source_path/" "$target_path/"

    git worktree remove "$temp_dir"

    git add "$target_path"
}

sync_module "libcontainer"
sync_module "libcgroups"

echo "Applying patch to comment out code in container_init_process.rs..."
patch -p1 < "./project/tools/scripts/container_init_process.patch" || {
    echo "Failed to apply patch"
    exit 1
}

git commit -m "Sync $module from youki"
echo "Changes committed for $module."

echo "Sync youki complete."
