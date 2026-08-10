# Triple-C model gateway — a LiteLLM proxy that speaks the Anthropic Messages
# API on the front and OpenAI (or any other LiteLLM provider) on the back.
#
# Claude Code only ever talks the Anthropic Messages API: it POSTs to
# `${ANTHROPIC_BASE_URL}/v1/messages`. OpenAI has no such route, so an OpenAI
# key cannot be pointed at Claude Code directly. LiteLLM's proxy exposes
# `/v1/messages` in Anthropic format and translates each request to the
# configured provider, which is what makes first-class OpenAI support possible.
#
# ── Why this image is built FROM the official LiteLLM image ──────────────────
# We deliberately do NOT `pip install litellm` ourselves. The official image is
# built by the LiteLLM maintainers from a locked dependency set and already
# carries the proxy entrypoint (`docker/prod_entrypoint.sh`), the admin UI and
# the Prisma bits. Hand-rolling a pip install would mean re-resolving the whole
# dependency tree on every build — exactly the surface that got poisoned in the
# incident below — and would drift from what upstream tests.
#
# ── Why the version is PINNED and why THIS version ───────────────────────────
# LiteLLM 1.82.7 and 1.82.8 were published to PyPI containing malware. Anything
# that resolves `litellm` at build time (or floats a `latest`/`main-latest` tag)
# can silently land on a compromised build, so the tag here is pinned to an
# exact release and additionally pinned by digest — a tag can be re-pushed, a
# digest cannot.
#
# The malicious wheels never became images — the official images build from a
# pinned requirements.txt and were confirmed unaffected (GHSA-5mg7-485q-xm76,
# https://docs.litellm.ai/blog/security-update-march-2026). The reason to pin is
# to keep it that way: a floating tag re-resolves, and this container ends up
# holding an OpenAI API key.
#
# v1.96.0 is chosen rather than the nearest clean post-incident release (1.83.0)
# because the intervening releases fix a run of *ordinary* proxy CVEs that matter
# for a gateway published on a host port: SQL injection in API-key verification
# (CVE-2026-42208, fixed 1.83.7), auth bypass via Host header injection
# (CVE-2026-49468, fixed 1.84.0) and MCP auth bypass (CVE-2026-59822, fixed
# 1.84.0). 1.84.0 is the floor; v1.96.0 is the newest release with no open OSV
# advisories at the time of writing. Note LiteLLM has retired the `main-*` tag
# family (`main-latest` is no longer updated, `main-stable` is deprecated) in
# favour of plain semver tags, which is why this is `v1.96.0` and not
# `main-v1.96.0-stable`.
#
# Bump deliberately, never automatically, and re-check the digest when you do.
FROM ghcr.io/berriai/litellm:v1.96.0@sha256:90d8de0ea6fbb3cad145d1019d00a0149ae400b1e18e2011a60f1988f143f672

# A placeholder config so the image is runnable on its own. Triple-C overwrites
# /etc/litellm/config.yaml (a named volume) with the generated one before the
# container is started for the first time — the generated file holds the
# provider API key, so it is never baked into an image layer.
COPY config.yaml /etc/litellm/config.yaml

EXPOSE 4000

# The base image's ENTRYPOINT is `docker/prod_entrypoint.sh`, which execs
# `litellm "$@"`. These are the arguments Triple-C also passes explicitly when
# it runs the unmodified upstream image instead of this one.
CMD ["--config", "/etc/litellm/config.yaml", "--host", "0.0.0.0", "--port", "4000"]
