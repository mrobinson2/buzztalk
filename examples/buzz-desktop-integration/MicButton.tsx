// Drop-in mic-button for Buzz's channel header (React + Tauri).
// Copy into desktop/src/features/chat/ui/ and render it in the composer/
// header, passing the active channel's id and the agent to speak.
//
// Behaviour: click to toggle voice. While on, live partial transcripts
// stream into the composer draft; the pipeline's state ("listening",
// "AgentSpeaking", ...) drives the button's look so the user can see it
// hearing / thinking / speaking. Interruption (barge-in) is automatic at
// the engine level — the user just talks over the reply.

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

type UiEvent =
  | { kind: "Partial"; text: string }
  | { kind: "Final"; text: string }
  | { kind: "Agent"; text: string }
  | { kind: "State"; text: string }
  | { kind: "AudioRebuilt"; text: string }
  | { kind: "Lost"; text: string };

interface Props {
  relayUrl: string;
  channelUuid: string;
  agentPubkey: string;
  keyFile: string; // app writes the user's nsec to an app-private file
  onPartial: (text: string) => void; // feed the composer draft
}

const isMac = navigator.platform.toLowerCase().includes("mac");

export function MicButton({ relayUrl, channelUuid, agentPubkey, keyFile, onPartial }: Props) {
  const [active, setActive] = useState(false);
  const [phase, setPhase] = useState<string>("");

  useEffect(() => {
    const un = listen<UiEvent>("buzztalk://event", (e) => {
      const ev = e.payload;
      if (ev.kind === "Partial") onPartial(ev.text);
      else if (ev.kind === "State") setPhase(ev.text);
    });
    return () => void un.then((f) => f());
  }, [onPartial]);

  async function toggle() {
    if (active) {
      await invoke("buzztalk_stop");
      setActive(false);
      setPhase("");
    } else {
      await invoke("buzztalk_start", {
        relayUrl,
        channelUuid,
        agentPubkey,
        keyFile,
        useVoiceProcessing: isMac, // VPIO on macOS; cpal elsewhere
      });
      setActive(true);
    }
  }

  const label = !active
    ? "Start voice"
    : phase.includes("AgentSpeaking")
      ? "Speaking… (talk to interrupt)"
      : phase.includes("Await") || phase.includes("Submit")
        ? "Thinking…"
        : "Listening…";

  return (
    <button
      type="button"
      aria-pressed={active}
      title={label}
      onClick={toggle}
      className={active ? "mic-button mic-button--active" : "mic-button"}
    >
      {active ? "●" : "🎙"} {label}
    </button>
  );
}
