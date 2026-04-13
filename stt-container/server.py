import os
import tempfile

from faster_whisper import WhisperModel
from fastapi import FastAPI, File, Form, UploadFile
from fastapi.responses import JSONResponse

app = FastAPI()
model: WhisperModel | None = None


@app.on_event("startup")
def load_model():
    global model
    model_size = os.environ.get("WHISPER_MODEL", "tiny")
    model = WhisperModel(model_size, device="cpu", compute_type="int8")


@app.post("/transcribe")
async def transcribe(
    file: UploadFile = File(...),
    language: str = Form(None),
):
    if model is None:
        return JSONResponse(status_code=503, content={"error": "Model not loaded"})

    with tempfile.NamedTemporaryFile(suffix=".wav", delete=True) as tmp:
        tmp.write(await file.read())
        tmp.flush()
        kwargs = {}
        if language:
            kwargs["language"] = language
        segments, info = model.transcribe(tmp.name, **kwargs)
        text = " ".join(s.text for s in segments).strip()

    return {"text": text, "language": info.language}


@app.get("/health")
def health():
    return {"status": "ok"}
