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
# Costs a few cents (about 40 Fargate task-minutes at 1 vCPU / 2 GB).
set -euo pipefail

: "${TRIPLEC_AWS_E2E_PROFILE:?set TRIPLEC_AWS_E2E_PROFILE (see aws/README.md, Live verification)}"
: "${TRIPLEC_AWS_E2E_REGION:?set TRIPLEC_AWS_E2E_REGION}"
root=$(cd "$(dirname "$0")/.." && pwd)
repo=triple-c-e2e-sandbox
tag="e2e-$(date -u +%Y%m%d%H%M%S)"
aws_() { aws --profile "$TRIPLEC_AWS_E2E_PROFILE" --region "$TRIPLEC_AWS_E2E_REGION" "$@"; }

docker_config=$(mktemp -d)
cleanup() {
    status=$?
    if aws_ ecr describe-repositories --repository-names "$repo" >/dev/null 2>&1; then
        aws_ ecr delete-repository --repository-name "$repo" --force >/dev/null \
            && echo "aws-e2e-workspace: deleted ECR repository $repo" \
            || echo "aws-e2e-workspace: WARNING: could not delete ECR repository $repo" >&2
    fi
    rm -rf "$docker_config"
    exit "$status"
}
trap cleanup EXIT INT TERM

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
