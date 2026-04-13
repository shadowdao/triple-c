FROM python:3.11-slim

RUN pip install --no-cache-dir \
    faster-whisper \
    fastapi \
    uvicorn[standard] \
    python-multipart

COPY server.py /app/server.py

EXPOSE 9876

CMD ["uvicorn", "app.server:app", "--host", "0.0.0.0", "--port", "9876"]
