import { useCallback, useRef, useState } from "react";
import * as commands from "../lib/tauri-commands";

type VoiceState = "inactive" | "starting" | "active" | "error";

export function useVoice(sessionId: string, deviceId?: string | null) {
  const [state, setState] = useState<VoiceState>("inactive");
  const [error, setError] = useState<string | null>(null);

  const audioContextRef = useRef<AudioContext | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const workletRef = useRef<AudioWorkletNode | null>(null);

  const start = useCallback(async () => {
    if (state === "active" || state === "starting") return;
    setState("starting");
    setError(null);

    try {
      // 1. Start the audio bridge in the container (creates FIFO writer)
      await commands.startAudioBridge(sessionId);

      // 2. Get microphone access (use specific device if configured)
      const audioConstraints: MediaTrackConstraints = {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
      };
      if (deviceId) {
        audioConstraints.deviceId = { exact: deviceId };
      }

      const stream = await navigator.mediaDevices.getUserMedia({
        audio: audioConstraints,
      });
      streamRef.current = stream;

      // 3. Create AudioContext at 16kHz (browser handles resampling)
      const audioContext = new AudioContext({ sampleRate: 16000 });
      audioContextRef.current = audioContext;

      // 4. Load AudioWorklet processor
      await audioContext.audioWorklet.addModule("/audio-capture-processor.js");

      // 5. Connect: mic → worklet → (silent) destination
      const source = audioContext.createMediaStreamSource(stream);
      const processor = new AudioWorkletNode(audioContext, "audio-capture-processor");
      workletRef.current = processor;

      // 6. Handle PCM chunks from the worklet
      processor.port.onmessage = (event: MessageEvent<ArrayBuffer>) => {
        const bytes = Array.from(new Uint8Array(event.data));
        commands.sendAudioData(sessionId, bytes).catch(() => {
          // Audio bridge may have been closed — ignore send errors
        });
      };

      source.connect(processor);
      processor.connect(audioContext.destination);

      setState("active");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setError(msg);
      setState("error");
      // Clean up on failure
      await commands.stopAudioBridge(sessionId).catch(() => {});
    }
  }, [sessionId, state, deviceId]);

  const stop = useCallback(async () => {
    // Tear down audio pipeline
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

    // Stop the container-side audio bridge
    await commands.stopAudioBridge(sessionId).catch(() => {});

    setState("inactive");
    setError(null);
  }, [sessionId]);

  const toggle = useCallback(async () => {
    if (state === "active") {
      await stop();
    } else {
      await start();
    }
  }, [state, start, stop]);

  return { state, error, start, stop, toggle };
}
