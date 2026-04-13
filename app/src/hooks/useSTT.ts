import { useCallback, useRef, useState } from "react";
import * as commands from "../lib/tauri-commands";
import { encodeWav } from "../lib/wav";
import { useAppState } from "../store/appState";

export type SttState = "idle" | "recording" | "transcribing" | "error";

export function useSTT(sessionId: string, sendInput: (sessionId: string, data: string) => Promise<void>) {
  const [state, setState] = useState<SttState>("idle");
  const [error, setError] = useState<string | null>(null);

  const audioContextRef = useRef<AudioContext | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const workletRef = useRef<AudioWorkletNode | null>(null);
  const chunksRef = useRef<Int16Array[]>([]);

  const appSettings = useAppState((s) => s.appSettings);
  const deviceId = appSettings?.default_microphone;

  const startRecording = useCallback(async () => {
    if (state === "recording" || state === "transcribing") return;
    setState("recording");
    setError(null);
    chunksRef.current = [];

    try {
      const audioConstraints: MediaTrackConstraints = {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      };
      if (deviceId) {
        audioConstraints.deviceId = { exact: deviceId };
      }

      const stream = await navigator.mediaDevices.getUserMedia({ audio: audioConstraints });
      streamRef.current = stream;

      const audioContext = new AudioContext({ sampleRate: 16000 });
      audioContextRef.current = audioContext;

      await audioContext.audioWorklet.addModule("/audio-capture-processor.js");

      const source = audioContext.createMediaStreamSource(stream);
      const processor = new AudioWorkletNode(audioContext, "audio-capture-processor");
      workletRef.current = processor;

      processor.port.onmessage = (event: MessageEvent<ArrayBuffer>) => {
        chunksRef.current.push(new Int16Array(event.data));
      };

      source.connect(processor);
      processor.connect(audioContext.destination);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setState("error");
    }
  }, [state, deviceId]);

  const stopRecording = useCallback(async () => {
    if (state !== "recording") return;

    // Stop audio capture
    workletRef.current?.disconnect();
    workletRef.current = null;

    if (audioContextRef.current) {
      await audioContextRef.current.close().catch(() => {});
      audioContextRef.current = null;
    }

    if (streamRef.current) {
      streamRef.current.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }

    // Concatenate PCM chunks
    const chunks = chunksRef.current;
    chunksRef.current = [];

    if (chunks.length === 0) {
      setState("idle");
      return;
    }

    const totalLength = chunks.reduce((sum, c) => sum + c.length, 0);
    const pcm = new Int16Array(totalLength);
    let offset = 0;
    for (const chunk of chunks) {
      pcm.set(chunk, offset);
      offset += chunk.length;
    }

    // Encode to WAV and transcribe
    setState("transcribing");
    try {
      const wavBlob = encodeWav(pcm, 16000);
      const wavBuffer = await wavBlob.arrayBuffer();
      const audioData = Array.from(new Uint8Array(wavBuffer));

      const text = await commands.transcribeAudio(audioData);
      if (text) {
        await sendInput(sessionId, text);
      }
      setState("idle");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setState("error");
      // Reset to idle after a brief delay so the UI shows the error
      setTimeout(() => setState("idle"), 3000);
    }
  }, [state, sessionId, sendInput]);

  const cancelRecording = useCallback(async () => {
    workletRef.current?.disconnect();
    workletRef.current = null;

    if (audioContextRef.current) {
      await audioContextRef.current.close().catch(() => {});
      audioContextRef.current = null;
    }

    if (streamRef.current) {
      streamRef.current.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
    }

    chunksRef.current = [];
    setState("idle");
    setError(null);
  }, []);

  const toggle = useCallback(async () => {
    if (state === "recording") {
      await stopRecording();
    } else if (state === "idle" || state === "error") {
      await startRecording();
    }
  }, [state, startRecording, stopRecording]);

  return { state, error, startRecording, stopRecording, cancelRecording, toggle };
}
