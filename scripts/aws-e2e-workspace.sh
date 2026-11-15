#!/usr/bin/env bash
# Run the phase-5 live test (`a_fargate_workspace_round_trips`) end to end:
# build the sandbox image from this checkout, push it to a throwaway private
# ECR repository, run the test, and delete the repository again — on success,
# failure or Ctrl-C (trap). The test deletes the repository in its own guard
# as well (TRIPLEC_E2E_ECR_REPO), so a crash of either side still removes it.
#
# Needs: docker, the AWS CLI, cargo, session-manager-plugin, and the admin
# profile of a DISPOSABLE account. Usage, from anywhere in the repo:
#
#   TRIPLEC_AWS_E2E_PROFILE=<profile> TRIPLEC_AWS_E2E_REGION=us-west-2 \
#     scripts/aws-e2e-workspace.sh
#
# Credentials come ONLY from that profile. Every other AWS_* variable of the
# caller (AWS_PROFILE, AWS_ACCESS_KEY_ID, AWS_SESSION_TOKEN, AWS_REGION,
# AWS_ENDPOINT_URL*, ...) is unset first, so ambient credentials can never
# end up running the test. Where the profile is read from:
#
#   TRIPLEC_AWS_E2E_CREDENTIALS_FILE  the shared credentials file (default:
#                                     the caller's AWS_SHARED_CREDENTIALS_FILE,
#                                     else ~/.aws/credentials)
#   TRIPLEC_AWS_E2E_CONFIG_FILE       the config file (default: the caller's
#                                     AWS_CONFIG_FILE, else ~/.aws/config)
#
# These two are re-exported as AWS_SHARED_CREDENTIALS_FILE / AWS_CONFIG_FILE,
# with AWS_PROFILE set to the profile; nothing else AWS_* is passed on.
#
# Costs a few cents (about 40 Fargate task-minutes at 1 vCPU / 2 GB).
set -euo pipefail

: "${TRIPLEC_AWS_E2E_PROFILE:?set TRIPLEC_AWS_E2E_PROFILE (see aws/README.md, Live verification)}"
: "${TRIPLEC_AWS_E2E_REGION:?set TRIPLEC_AWS_E2E_REGION}"
credentials_file="${TRIPLEC_AWS_E2E_CREDENTIALS_FILE:-${AWS_SHARED_CREDENTIALS_FILE:-}}"
config_file="${TRIPLEC_AWS_E2E_CONFIG_FILE:-${AWS_CONFIG_FILE:-}}"
while IFS= read -r v; do
    unset "$v"
done < <(compgen -e | grep '^AWS_' || true)
[ -z "$credentials_file" ] || export AWS_SHARED_CREDENTIALS_FILE="$credentials_file"
[ -z "$config_file" ] || export AWS_CONFIG_FILE="$config_file"
export AWS_PROFILE="$TRIPLEC_AWS_E2E_PROFILE" AWS_PAGER=""

root=$(cd "$(dirname "$0")/.." && pwd)
repo=triple-c-e2e-sandbox
tag="e2e-$(date -u +%Y%m%d%H%M%S)"
aws_() { aws --profile "$TRIPLEC_AWS_E2E_PROFILE" --region "$TRIPLEC_AWS_E2E_REGION" "$@"; }

docker_config=$(mktemp -d)
# The repository is deleted whatever happened before, even if this script
# never got as far as creating it: a "not found" is the only answer that is
# not worth a warning.
cleanup() {
    local status=$? out
    trap - EXIT INT TERM
    if out=$(timeout 120 aws ecr delete-repository --profile "$TRIPLEC_AWS_E2E_PROFILE" \
        --region "$TRIPLEC_AWS_E2E_REGION" --repository-name "$repo" --force 2>&1 >/dev/null); then
        echo "aws-e2e-workspace: deleted ECR repository $repo"
    elif [[ "$out" != *RepositoryNotFoundException* ]]; then
        echo "aws-e2e-workspace: WARNING: could not delete ECR repository $repo: $out" >&2
    fi
    rm -rf "$docker_config"
    exit "$status"
}
trap cleanup EXIT
# Ctrl-C / kill: the usual shell statuses, and the EXIT trap still cleans up.
trap 'exit 130' INT
trap 'exit 143' TERM

account=$(aws_ sts get-caller-identity --query Account --output text)
registry="$account.dkr.ecr.$TRIPLEC_AWS_E2E_REGION.amazonaws.com"
image="$registry/$repo:$tag"

docker build --platform linux/amd64 -t "$image" "$root/container"
aws_ ecr create-repository --repository-name "$repo" \
    --tags Key=triple-c-test,Value=phase5 >/dev/null
aws_ ecr get-login-password \
    | DOCKER_CONFIG="$docker_config" docker login --username AWS --password-stdin "$registry" >/dev/null
DOCKER_CONFIG="$docker_config" docker push "$image"
docker rmi "$image" >/dev/null || true

cd "$root/app/src-tauri"
mkdir -p ../dist
cargo build --locked
# A stack overflow aborts without running any guard: give the test thread room.
RUST_MIN_STACK=16777216 TRIPLEC_E2E_IMAGE="$image" TRIPLEC_E2E_ECR_REPO="$repo" \
    cargo test --locked aws::live_tests::environment::a_fargate_workspace_round_trips \
    -- --ignored --test-threads=1 --nocapture
