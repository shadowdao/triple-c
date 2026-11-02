FROM python:3.11-slim

# The python:3.11 images freeze the pip and setuptools that shipped with that
# CPython branch (pip 24.0, setuptools 79), and both have since collected
# advisories: pip path traversal / tar-symlink / index-URL handling
# (CVE-2025-8869, CVE-2026-1703, CVE-2026-3219, CVE-2026-6357, CVE-2026-8643,
# CVE-2026-13346) and setuptools sdist/legacy-wheel path issues
# (CVE-2026-59890, GHSA-xmj3-9gf7-h52w). Upgrade them first, so the installs
# below also run under a patched pip.
RUN pip install --no-cache-dir --upgrade "pip>=26.2" "setuptools>=84.1" \
    && pip install --no-cache-dir \
    faster-whisper \
    fastapi \
    uvicorn[standard] \
    python-multipart

COPY server.py /app/server.py

EXPOSE 9876

CMD ["uvicorn", "app.server:app", "--host", "0.0.0.0", "--port", "9876"]
