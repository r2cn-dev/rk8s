#!/bin/bash
set -e

# Set Git user configuration
echo "Setting up Git configuration..."
git config --global user.name "GitHub Actions"
git config --global user.email "actions@github.com"

# Add and fetch the latest youki repository
echo "Fetching the latest youki repository..."
if ! git remote | grep -q "youki"; then
    git remote add youki https://github.com/youki-dev/youki.git
fi
git fetch youki

# Sync libcontainer module
echo "Syncing libcontainer from youki..."
git checkout youki/main -- crates/libcontainer
rsync -a --delete crates/libcontainer/ project/libcontainer/
rm -rf crates/libcontainer
git add project/libcontainer
git commit -m "Sync libcontainer from youki" || echo "No changes to commit for libcontainer"
git push || echo "No changes to push for libcontainer"

# Sync libcgroups module
echo "Syncing libcgroups from youki..."
git checkout youki/main -- crates/libcgroups
rsync -a --delete crates/libcgroups/ project/libcgroups/
rm -rf crates/libcgroups
git add project/libcgroups
git commit -m "Sync libcgroups from youki" || echo "No changes to commit for libcgroups"
git push || echo "No changes to push for libcgroups"

# Clean up crates directory
echo "Cleaning up crates directory..."
rm -rf crates || true
git rm -rf --ignore-unmatch crates
git commit -m "Remove crates directory" || echo "No changes to commit for cleanup"
git push || echo "No changes to push for cleanup"

echo "Sync youki complete."
