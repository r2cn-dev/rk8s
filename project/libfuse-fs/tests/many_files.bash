#!/bin/bash

# Create a test directory if it doesn't exist
TEST_DIR="."

# Create 1025 files with one line of content
for i in $(seq 1 2025); do
    echo "This is content for file number $i" > "$TEST_DIR/file_$i.txt"
done

echo "Created 2025 files in $TEST_DIR directory"