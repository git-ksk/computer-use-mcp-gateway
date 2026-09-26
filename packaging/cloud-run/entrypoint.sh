#!/bin/sh
set -eu

source_root="${CUMG_V2_CLOUD_RUN_SECRET_MOUNT_DIR:-/var/run/cumg-mounted}"
private_root="${CUMG_V2_CLOUD_RUN_PRIVATE_DIR:-/run/cumg/private}"

umask 077
mkdir -p "$private_root"
chmod 700 "$private_root"

materialize() {
    source_rel="$1"
    target_name="$2"
    env_name="$3"

    source_path="$source_root/$source_rel"
    target_path="$private_root/$target_name"
    temp_path="$private_root/.${target_name}.tmp.$$"

    if [ ! -f "$source_path" ] || [ -L "$source_path" ]; then
        echo "required Cloud Run mounted file is missing or unsafe: $source_rel" >&2
        exit 78
    fi

    rm -f "$temp_path"
    if ! cp "$source_path" "$temp_path"; then
        rm -f "$temp_path"
        echo "failed to materialize required Cloud Run mounted file: $source_rel" >&2
        exit 78
    fi
    chmod 600 "$temp_path"
    mv -f "$temp_path" "$target_path"
    export "$env_name=$target_path"
}

materialize "hub/value" "hub.key" "CUMG_V2_HUB_SECRET_FILE"
materialize "grant/value" "grant.key" "CUMG_V2_GRANT_SECRET_FILE"
materialize "device/value" "device.pub" "CUMG_V2_DEVICE_PUBLIC_KEY_FILE"
materialize "postgres/value" "postgres.password" "CUMG_V2_POSTGRES_PASSWORD_FILE"
materialize "northbound-policy/value" "northbound-policy.json" "CUMG_V2_NORTHBOUND_POLICY_FILE"
materialize "handoff-policy/value" "handoff-policy.json" "CUMG_V2_HOSTED_HANDOFF_POLICY_FILE"
materialize "oauth-introspection/value" "oauth-introspection.secret" "CUMG_V2_OAUTH_INTROSPECTION_CLIENT_SECRET_FILE"

exec "$@"
