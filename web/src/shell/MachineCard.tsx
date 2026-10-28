import { api } from "@/api/client";
import { useAsk } from "@/api/hooks";
import { Identicon } from "@/components/identicon";
import { Lamp } from "@/components/lamp";

/**
 * This machine, at the foot of the sidebar.
 *
 * The one question the rest of the shell never answers at a glance: can
 * anybody reach me right now? So the card says it, beside the key drawn the
 * way every device row draws one, and a click goes to where it is changed.
 *
 * Read once, not polled. The switch that changes it lives on the Settings
 * screen, and that screen is where somebody is when it changes.
 */
export function MachineCard({ onOpen }: { onOpen: () => void }) {
  const identity = useAsk(() => api.identity(), []);
  const host = useAsk(() => api.host(), []);
  const on = host.data?.active === true;
  const managed = host.data?.managed !== false;

  return (
    <button
      type="button"
      className="machine-card"
      onClick={(e) => {
        e.stopPropagation();
        onOpen();
      }}
      aria-label="This machine: open its settings"
    >
      <span className="machine-card-key" aria-hidden>
        {identity.data ? <Identicon of={identity.data.key} size={18} /> : null}
      </span>
      <span className="machine-card-text">
        <span className="machine-card-name">This machine</span>
        {host.data && managed ? (
          <Lamp state={on ? "ok" : "off"} className="max-w-full" title={on ? "Paired devices can connect to this machine" : "Paired devices cannot connect to this machine right now"}>
            <span className="truncate">{on ? "Connections on" : "Connections off"}</span>
          </Lamp>
        ) : (
          <span className="machine-card-sub">{identity.data ? identity.data.key.slice(0, 9) : " "}</span>
        )}
      </span>
    </button>
  );
}
