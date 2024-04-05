#!/bin/bash
set -x
# Get the directory of the script
script_dir=$(dirname -- "$(readlink -f -- "$0")")

cargo_toml_path="$script_dir/../Cargo.toml"

# Use grep and awk to extract the name and version
name=$(awk -F'=' '/^\[package\]/ { in_package = 1 } in_package && /name/ { gsub(/[" ]/, "", $2); print $2; exit }' "$cargo_toml_path")
version=$(awk -F'=' '/^\[package\]/ { in_package = 1 } in_package && /version/ { gsub(/[" ]/, "", $2); print $2; exit }' "$cargo_toml_path")

get_platform_file_end() {
    machine=$(uname -m)
    kernel=$(uname -s)

    case $kernel in
        Darwin)
            echo 'universal-apple-darwin.zip'
            ;;
        Linux)
            case $machine in
                x86_64)
                    echo 'x86_64-linux-gnu.tar.gz'
                    ;;
                armv7l)
                    echo 'armv7-linux-gnueabihf.tar.gz'
                    ;;
                aarch64)
                    echo 'aarch64-linux-gnu.tar.gz'
                    ;;
                *)
                    echo "No self-compiled binary found and unsupported release-architecture: $machine" >&2
                    exit 1
                    ;;
            esac
            ;;
        *)
            echo "No self-compiled binary found and unsupported OS: $kernel" >&2
            exit 1
            ;;
    esac
}
platform_file_end=$(get_platform_file_end)
archive_file=$name-v$version-$platform_file_end

github_url="https://github.com/daywalker90/$name/releases/download/v$version/$archive_file"


# Download the archive using curl
if ! curl -L "$github_url" -o "$script_dir/$archive_file"; then
    echo "Error downloading the file from $github_url" >&2
    exit 1
fi

# Extract the contents
if [[ $archive_file == *.tar.gz ]]; then
    if ! tar -xzvf "$script_dir/$archive_file" -C "$script_dir"; then
        echo "Error extracting the contents of $archive_file" >&2
        exit 1
    fi
elif [[ $archive_file == *.zip ]]; then
    if ! unzip "$script_dir/$archive_file" -d "$script_dir"; then
        echo "Error extracting the contents of $archive_file" >&2
        exit 1
    fi
else
    echo "Unknown archive format or unsupported file extension: $archive_file" >&2
    exit 1
fi

# Function to check if a Python package is installed
check_package() {
    python_exec="$1"
    package_name="$2"
    if $python_exec -c "import $package_name" &> /dev/null; then
        return 0
    else
        return 1
    fi
}

proto_path="$script_dir/../proto"
if [ -d "$proto_path" ]; then
    # Check if the package is installed in the first Python executable
    if check_package "$TEST_DIR/bin/python3" "grpc"; then
        python_exec="$TEST_DIR/bin/python3"
    elif check_package "python3" "grpc"; then
        python_exec="python3"
    else
        echo "Error: Package 'grpcio' is not installed" >&2
        exit 1
    fi

    # Generate grpc files
    if ! "$python_exec" -m grpc_tools.protoc --proto_path="$proto_path" --python_out=$script_dir --grpc_python_out=$script_dir $proto_path/*.proto; then
        echo "Error generating grpc files" >&2
        exit 1
    fi
fi
