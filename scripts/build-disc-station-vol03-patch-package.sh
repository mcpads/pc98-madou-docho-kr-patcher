#!/bin/sh
set -eu

usage() {
    printf '%s\n' \
        "usage: $0 <disc-station-vol03-disk1.hdm> <pc98-fat12-patcher-tool> <output.zip>" >&2
}

if [ "$#" -ne 3 ]; then
    usage
    exit 2
fi

source_hdm_path=$1
patcher_tool_dir=$2
package_output_path=$3

resolve_existing_path() (
    requested_path=$1
    if [ ! -e "$requested_path" ]; then
        printf 'required input is missing: %s\n' "$requested_path" >&2
        exit 1
    fi
    requested_dir=$(dirname -- "$requested_path")
    requested_name=$(basename -- "$requested_path")
    resolved_dir=$(CDPATH= cd -- "$requested_dir" && pwd)
    printf '%s/%s\n' "$resolved_dir" "$requested_name"
)

source_hdm_path=$(resolve_existing_path "$source_hdm_path")
patcher_tool_dir=$(resolve_existing_path "$patcher_tool_dir")

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
project_dir=$(dirname -- "$script_dir")
plan_path="$project_dir/release/disc-station-vol03-disk1.plan.json"
expected_content_sha256=4e54d3fd997247653a774c9867908889a54a822aadd4fff767bfd5e89c465776

if [ ! -f "$plan_path" ]; then
    printf 'patch author plan is missing: %s\n' "$plan_path" >&2
    exit 1
fi

if [ ! -f "$patcher_tool_dir/patch-core/Cargo.toml" ]; then
    printf 'PC-98 FAT12 patcher tool is missing patch-core: %s\n' "$patcher_tool_dir" >&2
    exit 1
fi

if [ -e "$package_output_path" ]; then
    printf 'refusing to overwrite patch package: %s\n' "$package_output_path" >&2
    exit 1
fi

package_output_dir=$(dirname -- "$package_output_path")
package_output_name=$(basename -- "$package_output_path")
mkdir -p "$package_output_dir"
package_output_dir=$(CDPATH= cd -- "$package_output_dir" && pwd)
package_output_path="$package_output_dir/$package_output_name"

build_workspace=$(mktemp -d "$package_output_dir/.docho-fat12-package.XXXXXX")
remove_build_workspace() {
    case "$build_workspace" in
        "$package_output_dir"/.docho-fat12-package.*) rm -r -- "$build_workspace" ;;
        *) printf 'refusing to remove unexpected build workspace: %s\n' "$build_workspace" >&2 ;;
    esac
}
trap remove_build_workspace 0 HUP INT TERM

content_hdm_path="$build_workspace/content.hdm"
candidate_package_path="$build_workspace/candidate.zip"
repeated_package_path="$build_workspace/repeated.zip"
reapplied_hdm_path="$build_workspace/reapplied.hdm"
build_report_path="$build_workspace/content-build-report.json"

cd "$project_dir"
cargo run --locked --release -- build-translation-test-image \
    "$source_hdm_path" "$content_hdm_path" > "$build_report_path"

content_sha256=$(shasum -a 256 "$content_hdm_path" | awk '{print $1}')
if [ "$content_sha256" != "$expected_content_sha256" ]; then
    printf 'release content SHA-256 differs: expected %s, got %s\n' \
        "$expected_content_sha256" "$content_sha256" >&2
    exit 1
fi

create_package() {
    output_path=$1
    cargo run --locked --release \
        --manifest-path "$patcher_tool_dir/patch-core/Cargo.toml" \
        --bin pc98_patch_author -- create \
        "$plan_path" "$source_hdm_path" "$content_hdm_path" "$output_path"
}

create_package "$candidate_package_path"
create_package "$repeated_package_path"

if ! cmp -s "$candidate_package_path" "$repeated_package_path"; then
    printf '%s\n' 'repeated package creation produced different ZIP bytes' >&2
    exit 1
fi

cargo run --locked --release \
    --manifest-path "$patcher_tool_dir/patch-core/Cargo.toml" \
    --bin pc98_patch_author -- inspect "$candidate_package_path"

cargo run --locked --release \
    --manifest-path "$patcher_tool_dir/patch-core/Cargo.toml" \
    --bin pc98_patch_author -- apply \
    "$source_hdm_path" "$candidate_package_path" "$reapplied_hdm_path"

ln "$candidate_package_path" "$package_output_path"
cmp -s "$candidate_package_path" "$package_output_path"
shasum -a 256 "$package_output_path" "$reapplied_hdm_path"
