#!/bin/bash

# Check if base directory is provided as argument
if [ $# -eq 0 ]; then
    base_dir="./tree_structure"  # default value
else
    base_dir="$1/tree_structure"
fi

# Function to create random files in a directory
create_files() {
    local dir=$1
    # Create 1-3 random files in each directory
    local num_files=3
    
    for ((i=1; i<=num_files; i++)); do
        echo "#!/bin/bash\necho 'Hello'" > "$dir/script_${i}.sh" ;
        echo "#!/bin/bash\necho 'Hello'" > "$dir/.tmp_script_${i}.sh" ;
        mv "$dir/script_${i}.sh" "$dir/new_script_${i}.sh" ;
        mv "$dir/.tmp_script_${i}.sh" "$dir/.new_tmp_script_${i}.sh" ;
    done
}

# Function to create directory tree recursively
create_tree() {
    local base_dir=$1
    local depth=$2
    local max_depth=14
    
    if [ "$depth" -gt "$max_depth" ]; then
        return
    fi
    
    # Create 2-3 subdirectories at each level
    local num_dirs=3
    
    for ((i=1; i<=num_dirs; i++)); do
        local new_dir="${base_dir}/level${depth}_dir${i}"
        mkdir -p "$new_dir"
        create_files "$new_dir"
        create_tree "$new_dir" $((depth + 1))
    done
}

# Create base directory
mkdir -p "$base_dir"

# Start creating the tree from depth 1
create_tree "$base_dir" 1

echo "Directory tree created successfully in: $base_dir"