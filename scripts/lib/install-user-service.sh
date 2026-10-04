#!/usr/bin/env bash
# Shared helpers for installing cosmon user services.

# Reject bytes that cannot be represented safely in a unit directive.
cosmon_require_unit_text() {
    local label="$1" value="$2"
    if [[ -z "$value" || "$value" == *[$'\001'-$'\037'$'\177']* ]]; then
        echo "install-user-service: $label is empty or contains a control character" >&2
        return 1
    fi
}

# Quote one systemd command-line argument. Percent is doubled because unit
# specifiers are expanded after parsing; dollars are literal in this context.
cosmon_unit_quote() {
    local value="$1"
    cosmon_require_unit_text "unit argument" "$value" || return 1
    value="${value//\\/\\\\}"
    value="${value//\"/\\\"}"
    value="${value//%/%%}"
    printf '"%s"' "$value"
}

# Render a template whose replacement tokens each occupy the remainder of one
# directive line. Keeping the substitutions line-oriented avoids sed's path
# and replacement-string grammar entirely.
cosmon_render_unit() {
    local template="$1" exec_start="$2" environment_path="$3" home="$4"
    [[ -f "$template" ]] || {
        echo "install-user-service: template not found: $template" >&2
        return 1
    }
    local line
    while IFS= read -r line || [[ -n "$line" ]]; do
        case "$line" in
            ExecStart=__EXEC_START__) printf 'ExecStart=%s\n' "$exec_start" ;;
            Environment=__ENVIRONMENT_PATH__) printf 'Environment=%s\n' "$environment_path" ;;
            WorkingDirectory=__HOME__) printf 'WorkingDirectory=%s\n' "$home" ;;
            *) printf '%s\n' "$line" ;;
        esac
    done < "$template"
}

cosmon_require_user_manager() {
    command -v systemctl >/dev/null 2>&1 || {
        echo "install-user-service: systemctl is required for Linux user services" >&2
        return 1
    }
    if ! systemctl --user show-environment >/dev/null 2>&1; then
        echo "install-user-service: the user service manager is unreachable" >&2
        echo "install-user-service: enable systemd for this distribution, restart it, and verify 'systemctl --user status'" >&2
        return 1
    fi
}

cosmon_validate_unit() {
    command -v systemd-analyze >/dev/null 2>&1 || {
        echo "install-user-service: systemd-analyze is required to validate the unit" >&2
        return 1
    }
    systemd-analyze --user verify "$@"
}
