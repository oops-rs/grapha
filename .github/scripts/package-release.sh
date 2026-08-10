#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <tag> <asset-suffix>" >&2
  exit 1
fi

tag="$1"
asset_suffix="$2"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/../.." && pwd)"
dist_dir="${repo_root}/dist"
archive_stem="grapha-${tag}-${asset_suffix}"
staging_dir="${dist_dir}/${archive_stem}"
archive_path="${dist_dir}/${archive_stem}.tar.gz"
checksum_path="${dist_dir}/${archive_stem}.sha256"
binary_path="${repo_root}/target/release/grapha"
bridge_path="${repo_root}/target/release/libGraphaSwiftBridge.dylib"
bridge_exports_path="${script_dir}/swift-bridge-exports.txt"

if [[ ! -f "${binary_path}" ]]; then
  echo "missing grapha binary at ${binary_path}" >&2
  exit 1
fi

if [[ ! -f "${bridge_path}" ]]; then
  echo "missing Swift bridge dylib at ${bridge_path}" >&2
  exit 1
fi

if [[ ! -f "${bridge_exports_path}" ]]; then
  echo "missing Swift bridge export list at ${bridge_exports_path}" >&2
  exit 1
fi

rm -rf "${staging_dir}"
mkdir -p "${staging_dir}"

install -m 755 "${binary_path}" "${staging_dir}/grapha"
install -m 644 "${bridge_path}" "${staging_dir}/libGraphaSwiftBridge.dylib"
install -m 644 "${repo_root}/LICENSE" "${staging_dir}/LICENSE"

# SwiftPM leaves implementation symbols in release dylibs. Keep only the C FFI
# boundary and symbols required by the dynamic loader in the distributed copy;
# preserve the unstripped build artifact for local debugging.
actual_exports="${staging_dir}/.swift-bridge-exports.txt"
nm -gU "${staging_dir}/libGraphaSwiftBridge.dylib" \
  | awk '{print $3}' \
  | grep '^_grapha_' \
  | LC_ALL=C sort > "${actual_exports}"

if ! cmp -s "${bridge_exports_path}" "${actual_exports}"; then
  echo "Swift bridge exports do not match ${bridge_exports_path}" >&2
  diff -u "${bridge_exports_path}" "${actual_exports}" >&2 || true
  exit 1
fi

strip -u -r -s "${bridge_exports_path}" "${staging_dir}/libGraphaSwiftBridge.dylib"
codesign --force --sign - "${staging_dir}/libGraphaSwiftBridge.dylib"
rm "${actual_exports}"

rm -f "${archive_path}" "${checksum_path}"
tar -C "${dist_dir}" -czf "${archive_path}" "${archive_stem}"
shasum -a 256 "${archive_path}" > "${checksum_path}"
